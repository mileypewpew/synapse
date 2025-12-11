use axum::{
    extract::{State, Request},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Json, Response},
    routing::{get, post},
    Router,
};
use deadpool_redis::{Config, Pool, Runtime};
use deadpool_redis::redis::cmd;
use serde_json::{json, Value};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, error, debug, warn};

// Import from the synapse library
use synapse::event::Event;
use synapse::EVENT_STREAM_NAME;

#[derive(Clone)]
struct AppState {
    redis_pool: Pool,
    api_key: String,
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
    });

    // 4. Build Router with Auth Middleware
    let app = Router::new()
        .route("/health", get(health_check))
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
    // Skip auth for health check
    if req.uri().path() == "/health" {
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

async fn emit_event(
    State(state): State<Arc<AppState>>,
    Json(event): Json<Event>,
) -> StatusCode {
    debug!("Received event: {:?}", event);

    // Get connection from pool
    let mut conn = match state.redis_pool.get().await {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to get Redis connection: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // Serialize payload for storage
    let payload_str = match serde_json::to_string(&event.payload) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to serialize payload: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    // Push to Redis Stream (XADD)
    let result: Result<String, _> = cmd("XADD")
        .arg(EVENT_STREAM_NAME)
        .arg("*") // Auto-generate ID
        .arg("source")
        .arg(&event.source)
        .arg("eventType")
        .arg(&event.event_type)
        .arg("payload")
        .arg(payload_str)
        .query_async(&mut conn)
        .await;

    match result {
        Ok(id) => {
            info!("Event emitted: {}/{} -> ID: {}", event.source, event.event_type, id);
            StatusCode::ACCEPTED
        }
        Err(e) => {
            error!("Failed to push event to Redis Stream: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
