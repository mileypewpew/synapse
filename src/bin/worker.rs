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

use deadpool_redis::redis::{cmd, AsyncCommands, Value as RedisValue};
use deadpool_redis::redis::streams::{StreamReadOptions, StreamReadReply};
use deadpool_redis::{Config, Runtime};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

// Import from the synapse library
use synapse::effects::LogEffect;
use synapse::event::Event;
use synapse::router::Router;
use synapse::{DEFAULT_CONSUMER_GROUP, EVENT_STREAM_NAME};

/// Build the router with configured effect handlers.
///
/// This is where you register effects for different event types.
/// In a production system, this could be driven by configuration.
fn build_router() -> Router {
    let mut router = Router::new();

    // Register handlers for known event types
    // Each event type can have multiple effects

    // Default: Log all events that don't have specific handlers
    router.set_default(Arc::new(LogEffect::with_prefix("unhandled")));

    // User events
    router.on("user.created", Arc::new(LogEffect::with_prefix("user")));
    router.on("user.updated", Arc::new(LogEffect::with_prefix("user")));
    router.on("user.deleted", Arc::new(LogEffect::with_prefix("user")));

    // Game events
    router.on("game.started", Arc::new(LogEffect::with_prefix("game")));
    router.on("game.ended", Arc::new(LogEffect::with_prefix("game")));
    router.on("game.achievement", Arc::new(LogEffect::with_prefix("game")));
    router.on("player.joined", Arc::new(LogEffect::with_prefix("game")));
    router.on("player.left", Arc::new(LogEffect::with_prefix("game")));

    // System events
    router.on("system.health", Arc::new(LogEffect::with_prefix("system")));
    router.on("system.alert", Arc::new(LogEffect::with_prefix("system")));

    // Example: Add webhook for specific events (uncomment when needed)
    // use synapse::effects::WebhookEffect;
    // if let Ok(discord_url) = env::var("DISCORD_WEBHOOK_URL") {
    //     router.on("game.achievement", Arc::new(WebhookEffect::new(discord_url)));
    // }

    info!(
        handler_count = router.handler_count(),
        event_types = ?router.event_types(),
        "Router configured"
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
    drop(conn);

    // Processing loop
    info!(
        stream = %EVENT_STREAM_NAME,
        "Listening for events"
    );

    let mut events_processed: u64 = 0;
    let mut events_failed: u64 = 0;

    loop {
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

        let result: Result<StreamReadReply, _> =
            conn.xread_options(&[EVENT_STREAM_NAME], &[">"], &opts).await;

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
                                let _: Result<(), _> = conn
                                    .xack(EVENT_STREAM_NAME, &consumer_group, &[&id])
                                    .await;
                                continue;
                            }
                        };

                        debug!(
                            id = %id,
                            source = %event.source,
                            event_type = %event.event_type,
                            "Processing event"
                        );

                        // Dispatch through router
                        match router.dispatch(&event).await {
                            Ok(dispatch_result) => {
                                events_processed += 1;

                                if dispatch_result.is_success() {
                                    debug!(
                                        id = %id,
                                        effects_executed = dispatch_result.effects_executed,
                                        "Event processed successfully"
                                    );
                                } else {
                                    events_failed += dispatch_result.failure_count() as u64;
                                    warn!(
                                        id = %id,
                                        failures = dispatch_result.failure_count(),
                                        "Event processed with failures"
                                    );
                                }
                            }
                            Err(e) => {
                                events_failed += 1;
                                error!(
                                    id = %id,
                                    error = %e,
                                    "Event dispatch failed"
                                );
                            }
                        }

                        // ACK the message
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
        if events_processed > 0 && events_processed % 100 == 0 {
            info!(
                events_processed = events_processed,
                events_failed = events_failed,
                "Worker statistics"
            );
        }
    }

    #[allow(unreachable_code)]
    Ok(())
}
