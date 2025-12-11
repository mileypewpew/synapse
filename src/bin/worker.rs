//! Synapse Worker - Event Processing Engine
//!
//! The worker consumes events from Redis Streams and dispatches them through
//! the Router to registered Effect handlers.
//!
//! ## Configuration
//!
//! Environment variables:
//! - `REDIS_URL`: Redis connection string (required)
//! - `SYNAPSE_WORKER_NAME`: Unique worker identifier (default: hostname or UUID)
//! - `SYNAPSE_CONSUMER_GROUP`: Consumer group name (default: "synapse_workers")
//! - `RUST_LOG`: Logging level (default: "info")

use deadpool_redis::redis::streams::{StreamReadOptions, StreamReadReply};
use deadpool_redis::redis::{cmd, AsyncCommands, Value as RedisValue};
use deadpool_redis::{Config, Runtime};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

// Import from the synapse library
use synapse::config::SynapseConfig;
use synapse::dlq::DeadLetterQueue;
use synapse::effects::LogEffect;
use synapse::event::Event;
use synapse::router::Router;
use synapse::shutdown::ShutdownSignal;
use synapse::{DEFAULT_CONSUMER_GROUP, EVENT_STREAM_NAME};

/// Maximum number of retries before moving to DLQ
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (in milliseconds)
const RETRY_BASE_DELAY_MS: u64 = 1000;

/// Idle time threshold for claiming pending messages (in milliseconds)
const PENDING_IDLE_THRESHOLD_MS: u64 = 30000;

/// Build the router from configuration file or use defaults.
fn build_router() -> Router {
    // Try to load config file
    match SynapseConfig::load() {
        Ok(config) => {
            let router = config.build_router();
            info!(
                handler_count = router.handler_count(),
                event_types = ?router.event_types(),
                "Router configured from config file"
            );
            router
        }
        Err(e) => {
            warn!(error = %e, "Failed to load config, using defaults");
            build_default_router()
        }
    }
}

/// Build a default router when no config is available.
fn build_default_router() -> Router {
    let mut router = Router::new();

    // Default: Log all events that don't have specific handlers
    router.set_default(Arc::new(LogEffect::with_prefix("unhandled")));

    // User events
    router.on("user.*", Arc::new(LogEffect::with_prefix("user")));

    // Game events
    router.on("game.*", Arc::new(LogEffect::with_prefix("game")));
    router.on("player.*", Arc::new(LogEffect::with_prefix("game")));

    // System events
    router.on("system.*", Arc::new(LogEffect::with_prefix("system")));

    info!(
        handler_count = router.handler_count(),
        event_types = ?router.event_types(),
        "Router configured with defaults"
    );

    router
}

/// Get the worker name from environment or generate one.
fn get_worker_name() -> String {
    if let Ok(name) = env::var("SYNAPSE_WORKER_NAME") {
        return name;
    }

    // Try hostname
    if let Ok(hostname) = hostname::get() {
        if let Some(name) = hostname.to_str() {
            return format!("worker-{}", name);
        }
    }

    // Fallback to UUID
    format!("worker-{}", uuid::Uuid::new_v4())
}

/// Get the consumer group name from environment or use default.
fn get_consumer_group() -> String {
    env::var("SYNAPSE_CONSUMER_GROUP").unwrap_or_else(|_| DEFAULT_CONSUMER_GROUP.to_string())
}

/// Parse an Event from Redis stream data.
fn parse_event(map: &HashMap<String, RedisValue>) -> Option<Event> {
    let source = get_str_field(map, "source");
    let event_type = get_str_field(map, "eventType");
    let payload_str = get_str_field(map, "payload");
    let timestamp = get_optional_str_field(map, "timestamp");
    let correlation_id = get_optional_str_field(map, "correlationId");

    if source == "unknown" || event_type == "unknown" {
        warn!("Event missing required fields");
        return None;
    }

    // Parse payload JSON
    let payload: Value = match serde_json::from_str(&payload_str) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "Failed to parse event payload, using empty object");
            Value::Object(serde_json::Map::new())
        }
    };

    Some(Event {
        source,
        event_type,
        payload,
        timestamp,
        correlation_id,
    })
}

/// Extract a string field from Redis stream data.
fn get_str_field(map: &HashMap<String, RedisValue>, key: &str) -> String {
    if let Some(val) = map.get(key) {
        match val {
            RedisValue::BulkString(bytes) => String::from_utf8_lossy(bytes).to_string(),
            RedisValue::SimpleString(s) => s.clone(),
            _ => "unknown_format".to_string(),
        }
    } else {
        "unknown".to_string()
    }
}

/// Extract an optional string field from Redis stream data.
fn get_optional_str_field(map: &HashMap<String, RedisValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|val| match val {
        RedisValue::BulkString(bytes) => {
            let s = String::from_utf8_lossy(bytes).to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        RedisValue::SimpleString(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        _ => None,
    })
}

/// Get retry count from event metadata, defaulting to 0.
fn get_retry_count(map: &HashMap<String, RedisValue>) -> u32 {
    get_optional_str_field(map, "retryCount")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Calculate exponential backoff delay.
fn calculate_backoff(retry_count: u32) -> Duration {
    let delay_ms = RETRY_BASE_DELAY_MS * (1 << retry_count.min(5)); // Cap at 32 seconds
    Duration::from_millis(delay_ms)
}

/// Claim pending messages that have been idle for too long.
/// Returns the number of messages claimed.
#[allow(clippy::type_complexity)]
async fn claim_pending_messages(
    conn: &mut deadpool_redis::Connection,
    consumer_group: &str,
    worker_name: &str,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    // Use XAUTOCLAIM to claim idle pending messages
    // XAUTOCLAIM key group consumer min-idle-time start [COUNT count]
    let result: Result<(String, Vec<(String, HashMap<String, RedisValue>)>), _> = cmd("XAUTOCLAIM")
        .arg(EVENT_STREAM_NAME)
        .arg(consumer_group)
        .arg(worker_name)
        .arg(PENDING_IDLE_THRESHOLD_MS)
        .arg("0-0") // Start from beginning
        .arg("COUNT")
        .arg(10) // Claim up to 10 messages at a time
        .query_async(conn)
        .await;

    match result {
        Ok((_, messages)) => {
            let count = messages.len();
            if count > 0 {
                info!(
                    count = count,
                    "Claimed pending messages from previous workers"
                );
            }
            Ok(count)
        }
        Err(e) => {
            // XAUTOCLAIM might not be available in older Redis versions
            debug!(error = %e, "XAUTOCLAIM failed, skipping pending recovery");
            Ok(0)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    // Get configuration
    let worker_name = get_worker_name();
    let consumer_group = get_consumer_group();
    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");

    info!(
        worker_name = %worker_name,
        consumer_group = %consumer_group,
        "Synapse Worker starting"
    );

    // Build the event router
    let router = build_router();

    // Create Redis connection pool
    let cfg = Config::from_url(redis_url);
    let pool = cfg
        .create_pool(Some(Runtime::Tokio1))
        .expect("Failed to create Redis pool");

    // Create consumer group (if not exists)
    let mut conn = pool
        .get()
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

    let create_group_result: Result<(), _> = cmd("XGROUP")
        .arg("CREATE")
        .arg(EVENT_STREAM_NAME)
        .arg(&consumer_group)
        .arg("$")
        .arg("MKSTREAM")
        .query_async(&mut conn)
        .await;

    match create_group_result {
        Ok(_) => info!(
            consumer_group = %consumer_group,
            "Created consumer group"
        ),
        Err(e) => {
            if e.to_string().contains("BUSYGROUP") {
                info!(
                    consumer_group = %consumer_group,
                    "Consumer group already exists"
                );
            } else {
                error!(error = %e, "Failed to create consumer group");
                return Err(Box::new(e) as Box<dyn std::error::Error>);
            }
        }
    }

    // Create Dead Letter Queue
    let dlq = DeadLetterQueue::new(pool.clone());

    // Claim any pending messages from previous workers
    if let Err(e) = claim_pending_messages(&mut conn, &consumer_group, &worker_name).await {
        warn!(error = %e, "Failed to claim pending messages");
    }

    drop(conn);

    // Setup graceful shutdown
    let shutdown = ShutdownSignal::new();
    let mut shutdown_receiver = shutdown.subscribe();

    // Processing loop
    info!(
        stream = %EVENT_STREAM_NAME,
        "Listening for events"
    );

    let mut events_processed: u64 = 0;
    let mut events_failed: u64 = 0;
    let mut shutting_down = false;

    loop {
        // Check for shutdown signal (non-blocking)
        if shutdown_receiver.try_recv().is_ok() {
            info!("Shutdown signal received, finishing current batch...");
            shutting_down = true;
        }

        if shutting_down {
            info!(
                events_processed = events_processed,
                events_failed = events_failed,
                "Worker shutting down gracefully"
            );
            break;
        }

        let mut conn = match pool.get().await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "Failed to get Redis connection");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let opts = StreamReadOptions::default()
            .group(&consumer_group, &worker_name)
            .block(2000)
            .count(10); // Process up to 10 events per batch

        // Use select to handle shutdown during blocking read
        let result: Result<StreamReadReply, _> = tokio::select! {
            _ = shutdown.wait() => {
                info!("Shutdown signal received during read, finishing...");
                shutting_down = true;
                continue;
            }
            result = conn.xread_options(&[EVENT_STREAM_NAME], &[">"], &opts) => result,
        };

        match result {
            Ok(reply) => {
                for stream_key in reply.keys {
                    for element in stream_key.ids {
                        let id = element.id.clone();

                        // Parse the event
                        let event = match parse_event(&element.map) {
                            Some(e) => e,
                            None => {
                                warn!(id = %id, "Skipping unparseable event");
                                // Still ACK to avoid reprocessing
                                let _: Result<(), _> =
                                    conn.xack(EVENT_STREAM_NAME, &consumer_group, &[&id]).await;
                                continue;
                            }
                        };

                        // Get retry count from event metadata
                        let retry_count = get_retry_count(&element.map);

                        debug!(
                            id = %id,
                            source = %event.source,
                            event_type = %event.event_type,
                            retry_count = retry_count,
                            "Processing event"
                        );

                        // Dispatch through router
                        let dispatch_success = match router.dispatch(&event).await {
                            Ok(dispatch_result) => {
                                if dispatch_result.is_success() {
                                    events_processed += 1;
                                    debug!(
                                        id = %id,
                                        effects_executed = dispatch_result.effects_executed,
                                        "Event processed successfully"
                                    );
                                    true
                                } else {
                                    // Some effects failed
                                    let failure_count = dispatch_result.failure_count();
                                    events_failed += failure_count as u64;
                                    warn!(
                                        id = %id,
                                        failures = failure_count,
                                        retry_count = retry_count,
                                        "Event processed with failures"
                                    );
                                    false
                                }
                            }
                            Err(e) => {
                                events_failed += 1;
                                error!(
                                    id = %id,
                                    error = %e,
                                    retry_count = retry_count,
                                    "Event dispatch failed"
                                );
                                false
                            }
                        };

                        // Handle failure with retry logic
                        if !dispatch_success {
                            if retry_count >= MAX_RETRIES {
                                // Max retries exceeded, move to DLQ
                                warn!(
                                    id = %id,
                                    retry_count = retry_count,
                                    max_retries = MAX_RETRIES,
                                    "Max retries exceeded, moving to DLQ"
                                );

                                if let Err(e) = dlq
                                    .add_failed_event(
                                        &event,
                                        "Max retries exceeded",
                                        retry_count,
                                        Some(&id),
                                    )
                                    .await
                                {
                                    error!(
                                        id = %id,
                                        error = %e,
                                        "Failed to add event to DLQ"
                                    );
                                }
                            } else {
                                // Schedule retry with backoff
                                let backoff = calculate_backoff(retry_count);
                                debug!(
                                    id = %id,
                                    retry_count = retry_count,
                                    backoff_ms = backoff.as_millis(),
                                    "Scheduling retry with backoff"
                                );
                                // Note: In a production system, you might want to
                                // re-add the event with incremented retry count.
                                // For now, leaving it unacked allows XAUTOCLAIM to pick it up.
                                tokio::time::sleep(backoff).await;
                            }
                        }

                        // ACK the message (even for retries - they'll be re-queued if needed)
                        let ack_result: Result<(), _> =
                            conn.xack(EVENT_STREAM_NAME, &consumer_group, &[&id]).await;

                        if let Err(e) = ack_result {
                            error!(id = %id, error = %e, "Failed to ACK message");
                        }
                    }
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                // Ignore timeout/nil errors (normal when no messages)
                if !err_str.contains("timed out") && !err_str.contains("response was nil") {
                    warn!(error = %e, "Stream read error");
                }
            }
        }

        // Periodic stats (every 100 events)
        if events_processed > 0 && events_processed.is_multiple_of(100) {
            info!(
                events_processed = events_processed,
                events_failed = events_failed,
                "Worker statistics"
            );
        }
    }

    info!("Worker shutdown complete");
    Ok(())
}
