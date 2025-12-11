use deadpool_redis::{Config, Runtime};
use deadpool_redis::redis::{cmd, AsyncCommands, Value as RedisValue};
use deadpool_redis::redis::streams::{StreamReadOptions, StreamReadReply};
use std::env;
use std::time::Duration;
use tracing::{info, error, warn};

const EVENT_STREAM_NAME: &str = "synapse:events";
const CONSUMER_GROUP: &str = "synapse_workers";
const CONSUMER_NAME: &str = "worker-1";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    dotenvy::dotenv().ok();

    info!("Synapse Worker Starting...");

    let redis_url = env::var("REDIS_URL").expect("REDIS_URL must be set");
    let cfg = Config::from_url(redis_url);
    let pool = cfg.create_pool(Some(Runtime::Tokio1)).expect("Failed to create Redis pool");

    // 1. Create Consumer Group (if not exists)
    // Use .map_err to convert PoolError to Box<dyn Error>
    let mut conn = pool.get().await.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    
    let create_group_result: Result<(), _> = cmd("XGROUP")
        .arg("CREATE")
        .arg(EVENT_STREAM_NAME)
        .arg(CONSUMER_GROUP)
        .arg("$") 
        .arg("MKSTREAM")
        .query_async(&mut conn)
        .await;

    match create_group_result {
        Ok(_) => info!("Created consumer group '{}'", CONSUMER_GROUP),
        Err(e) => {
            if e.to_string().contains("BUSYGROUP") {
                info!("Consumer group '{}' already exists", CONSUMER_GROUP);
            } else {
                error!("Failed to create consumer group: {}", e);
                // Explicit cast to satisfy the return type
                return Err(Box::new(e) as Box<dyn std::error::Error>);
            }
        }
    }
    drop(conn);

    // 2. Processing Loop
    info!("Listening for events on stream '{}'...", EVENT_STREAM_NAME);
    
    loop {
        let mut conn = match pool.get().await {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to get Redis connection: {}", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let opts = StreamReadOptions::default()
            .group(CONSUMER_GROUP, CONSUMER_NAME)
            .block(2000)
            .count(1);

        let result: Result<StreamReadReply, _> = conn.xread_options(
            &[EVENT_STREAM_NAME],
            &[">"],
            &opts
        ).await;

        match result {
            Ok(reply) => {
                for stream_key in reply.keys {
                    for element in stream_key.ids {
                        let id = element.id;
                        let map = element.map;

                        let source = get_str_field(&map, "source");
                        let event_type = get_str_field(&map, "eventType");
                        // let payload = get_str_field(&map, "payload");

                        info!("Processing Event [{}]: {} - {}", id, source, event_type);
                        
                        // ACK
                        let ack_result: Result<(), _> = conn.xack(EVENT_STREAM_NAME, CONSUMER_GROUP, &[&id]).await;
                        if let Err(e) = ack_result {
                            error!("Failed to ACK message {}: {}", id, e);
                        }
                    }
                }
            }
            Err(e) => {
                let err_str = e.to_string();
                if !err_str.contains("timed out") && !err_str.contains("response was nil") {
                     warn!("Stream read error: {}", e); 
                }
            }
        }
    }
    
    // This part is technically unreachable because of the loop, but satisfies the compiler
    // regarding the return type of the function.
    #[allow(unreachable_code)]
    Ok(())
}

fn get_str_field(map: &std::collections::HashMap<String, RedisValue>, key: &str) -> String {
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