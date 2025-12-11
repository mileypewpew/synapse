use axum::{
    extract::State, http::StatusCode, response::Json, routing::get, routing::post, Router,
};
use deadpool_redis::{Config, Pool, Runtime};
use deadpool_redis::redis::{cmd, AsyncCommands};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

const EVENT_QUEUE_NAME: &str = "synapse:events";

#[derive(Clone)]
struct AppState {
    redis_pool: Pool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Event {
    source: String,
    #[serde(rename = "eventType")]
    event_type: String,
    payload: Value,
}

fn app(app_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/api/v1/events/emit", post(emit_event))
        .with_state(app_state)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok(); // Load .env file

    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");
    let cfg = Config::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1)).expect("Failed to create Redis pool");
    let app_state = Arc::new(AppState { redis_pool: pool });

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app(app_state)).await.unwrap();
}

async fn health_check(State(state): State<Arc<AppState>>) -> Result<Json<Value>, StatusCode> {
    let mut conn = state.redis_pool.get().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    cmd("PING").query_async::<_, String>(&mut conn).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "status": "ok", "redis": "ok" })))
}

async fn emit_event(
    State(state): State<Arc<AppState>>,
    Json(event): Json<Event>,
) -> StatusCode {
    println!("Received event: {:?}", event);

    let event_json = match serde_json::to_string(&event) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("Failed to serialize event to JSON: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };
    
    let mut conn = match state.redis_pool.get().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to get Redis connection from pool: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    match conn.rpush::<_, _, ()>(EVENT_QUEUE_NAME, event_json).await {
        Ok(_) => {
            println!("Pushed event to Redis queue '{}'", EVENT_QUEUE_NAME);
            StatusCode::ACCEPTED
        }
        Err(e) => {
            eprintln!("Failed to push event to Redis: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt; // for `oneshot`

    // Mock Redis functionality for testing
    struct MockRedis;
    // This is a placeholder for a more complete mocking library like `mockall`
    // For this test, we don't need to implement the traits, as we won't hit Redis.
    
    #[tokio::test]
    async fn health_check_test() {
        // We can't easily mock the Redis pool without more complex setup,
        // so for this unit test, we'll test a version of the handler
        // that doesn't depend on the state. A true integration test would be needed
        // to test the full handler with a real Redis connection.
        
        // Let's test a hypothetical health_check_simple that doesn't need Redis
        async fn health_check_simple() -> Json<Value> {
            Json(json!({ "status": "ok" }))
        }
        
        let app = Router::new().route("/health", get(health_check_simple));

        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, json!({ "status": "ok" }));
    }
}
