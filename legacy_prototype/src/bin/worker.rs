use deadpool_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;

const EVENT_QUEUE_NAME: &str = "synapse:events";

#[derive(Debug, Deserialize, Serialize)]
pub struct Event {
    source: String,
    #[serde(rename = "eventType")]
    event_type: String,
    payload: Value,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");

    let client = deadpool_redis::redis::Client::open(redis_url)?;
    let mut con = client.get_async_connection().await?;

    println!("Worker started, waiting for events...");

    loop {
        let popped_value: Option<(String, String)> = con.blpop(EVENT_QUEUE_NAME, 0.0).await?;

        if let Some((_queue, event_json)) = popped_value {
            match serde_json::from_str::<Event>(&event_json) {
                Ok(event) => {
                    println!("Processing event: {:?}", event);
                    // In the future, this is where plugin logic would go
                }
                Err(e) => {
                    eprintln!("Failed to deserialize event: {}", e);
                    eprintln!("Raw JSON: {}", event_json);
                }
            }
        }
    }
}
