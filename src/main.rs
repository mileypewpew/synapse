use axum::{
    extract::{State, Request},
    http::{StatusCode, header, HeaderMap},
    middleware::{self, Next},
    response::{Json, Response},
    routing::{get, post},
    Router,
};
use chrono::Utc;
use deadpool_redis::{Config, Pool, Runtime};
use deadpool_redis::redis::cmd;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tracing::{info, error, debug, warn};

// Import from the synapse library
use synapse::event::Event;
use synapse::EVENT_STREAM_NAME;

/// Application metrics
struct Metrics {
    events_received: AtomicU64,
    start_time: Instant,
}

impl Metrics {
    fn new() -> Self {
        Self {
            events_received: AtomicU64::new(0),
            start_time: Instant::now(),
        }
    }

    fn increment_events(&self) {
        self.events_received.fetch_add(1, Ordering::Relaxed);
    }

    fn uptime_seconds(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }
}

#[derive(Clone)]
struct AppState {
    redis_pool: Pool,
    api_key: String,
    metrics: Arc<Metrics>,
}

/// Response returned when an event is successfully accepted.
#[derive(Debug, Serialize, Deserialize)]
struct EventResponse {
    /// Redis stream ID assigned to the event
    id: String,
    /// Status of the request
    status: String,
    /// Correlation ID for tracing
    #[serde(rename = "correlationId")]
    correlation_id: String,
}

#[tokio::main]
async fn main() {
    // 1. Initialize Logging
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    // 2. Setup Configuration
    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");
    let api_key = env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| {
        warn!("SYNAPSE_API_KEY not set, defaulting to 'dev-key'. DO NOT USE IN PRODUCTION.");
        "dev-key".to_string()
    });

    // 3. Setup Redis Pool
    let cfg = Config::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1)).expect("Failed to create Redis pool");

    let app_state = Arc::new(AppState {
        redis_pool: pool,
        api_key,
        metrics: Arc::new(Metrics::new()),
    });

    // 4. Build Router with Auth Middleware
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(get_metrics))
        .route("/api/v1/events", post(emit_event))
        .layer(middleware::from_fn_with_state(app_state.clone(), auth_middleware))
        .with_state(app_state);

    // 5. Start Server
    let port = env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr_str = format!("0.0.0.0:{}", port);
    let addr: SocketAddr = addr_str.parse().expect("Invalid address");

    info!("Synapse Server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Skip auth for health check and metrics
    let path = req.uri().path();
    if path == "/health" || path == "/metrics" {
        return Ok(next.run(req).await);
    }

    let auth_header = req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok());

    match auth_header {
        Some(auth_header) if auth_header.starts_with("Bearer ") => {
            let token = &auth_header[7..];
            if token == state.api_key {
                Ok(next.run(req).await)
            } else {
                warn!("Invalid API Key attempt");
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        _ => {
            warn!("Missing or malformed Authorization header");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

async fn health_check(State(state): State<Arc<AppState>>) -> Result<Json<Value>, StatusCode> {
    let mut conn = state.redis_pool.get().await.map_err(|e| {
        error!("Failed to get Redis connection: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Simple PING to check Redis connectivity
    let _: String = cmd("PING").query_async(&mut conn).await.map_err(|e| {
        error!("Redis PING failed: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(json!({ "status": "ok", "redis": "connected" })))
}

/// Get metrics endpoint - returns server metrics as JSON
async fn get_metrics(State(state): State<Arc<AppState>>) -> Json<Value> {
    let uptime = state.metrics.uptime_seconds();
    let events_received = state.metrics.events_received.load(Ordering::Relaxed);

    // Format uptime as human-readable
    let uptime_str = if uptime < 60 {
        format!("{}s", uptime)
    } else if uptime < 3600 {
        format!("{}m {}s", uptime / 60, uptime % 60)
    } else {
        format!("{}h {}m {}s", uptime / 3600, (uptime % 3600) / 60, uptime % 60)
    };

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": uptime,
        "uptime": uptime_str,
        "events": {
            "received": events_received
        },
        "status": "running"
    }))
}

async fn emit_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(event): Json<Event>,
) -> Result<(StatusCode, Json<EventResponse>), StatusCode> {
    debug!("Received event: {:?}", event);

    // Track metrics
    state.metrics.increment_events();

    // Extract or generate correlation ID
    let correlation_id = headers
        .get("X-Correlation-ID")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Generate timestamp (ISO 8601)
    let timestamp = Utc::now().to_rfc3339();

    debug!(
        correlation_id = %correlation_id,
        timestamp = %timestamp,
        "Event metadata"
    );

    // Get connection from pool
    let mut conn = state.redis_pool.get().await.map_err(|e| {
        error!("Failed to get Redis connection: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Serialize payload for storage
    let payload_str = serde_json::to_string(&event.payload).map_err(|e| {
        error!("Failed to serialize payload: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // Push to Redis Stream (XADD) with metadata
    let id: String = cmd("XADD")
        .arg(EVENT_STREAM_NAME)
        .arg("*") // Auto-generate ID
        .arg("source")
        .arg(&event.source)
        .arg("eventType")
        .arg(&event.event_type)
        .arg("payload")
        .arg(payload_str)
        .arg("timestamp")
        .arg(&timestamp)
        .arg("correlationId")
        .arg(&correlation_id)
        .query_async(&mut conn)
        .await
        .map_err(|e| {
            error!("Failed to push event to Redis Stream: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!(
        id = %id,
        source = %event.source,
        event_type = %event.event_type,
        correlation_id = %correlation_id,
        "Event emitted"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(EventResponse {
            id,
            status: "accepted".to_string(),
            correlation_id,
        }),
    ))
}
