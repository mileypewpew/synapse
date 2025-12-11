//! Dead Letter Queue for failed events.
//!
//! When events fail processing after maximum retries, they are moved to a
//! Dead Letter Queue (DLQ) for manual investigation and potential retry.
//!
//! # Example
//!
//! ```rust,ignore
//! use synapse::dlq::DeadLetterQueue;
//!
//! let dlq = DeadLetterQueue::new(redis_pool);
//! dlq.add_failed_event(event, "WebhookEffect: connection timeout", 3).await?;
//!
//! // Later, list failed events
//! let failed = dlq.list(10, 0).await?;
//! ```

use deadpool_redis::redis::cmd;
use deadpool_redis::Pool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, error, info};

use crate::event::Event;

/// Redis stream name for the Dead Letter Queue
pub const DLQ_STREAM_NAME: &str = "synapse:events:dlq";

/// Maximum entries to keep in DLQ (older entries are trimmed)
const DLQ_MAX_LEN: usize = 10000;

/// A failed event stored in the Dead Letter Queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedEvent {
    /// Original event data
    pub event: Event,

    /// Error message from the last failed attempt
    pub error: String,

    /// Number of retry attempts before moving to DLQ
    pub retry_count: u32,

    /// ISO 8601 timestamp when the event was moved to DLQ
    pub failed_at: String,

    /// Original Redis stream ID (if available)
    pub original_id: Option<String>,
}

/// Dead Letter Queue for storing failed events.
#[derive(Clone)]
pub struct DeadLetterQueue {
    pool: Pool,
}

impl DeadLetterQueue {
    /// Create a new Dead Letter Queue instance.
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Add a failed event to the DLQ.
    pub async fn add_failed_event(
        &self,
        event: &Event,
        error: &str,
        retry_count: u32,
        original_id: Option<&str>,
    ) -> Result<String, DlqError> {
        let mut conn = self.pool.get().await.map_err(|e| {
            error!(error = %e, "Failed to get Redis connection for DLQ");
            DlqError::ConnectionError(e.to_string())
        })?;

        let failed_at = chrono::Utc::now().to_rfc3339();

        // Serialize the event payload
        let event_json = serde_json::to_string(event).map_err(|e| {
            error!(error = %e, "Failed to serialize event for DLQ");
            DlqError::SerializationError(e.to_string())
        })?;

        // Add to DLQ stream with MAXLEN to prevent unbounded growth
        let id: String = cmd("XADD")
            .arg(DLQ_STREAM_NAME)
            .arg("MAXLEN")
            .arg("~")
            .arg(DLQ_MAX_LEN)
            .arg("*")
            .arg("event")
            .arg(&event_json)
            .arg("error")
            .arg(error)
            .arg("retryCount")
            .arg(retry_count)
            .arg("failedAt")
            .arg(&failed_at)
            .arg("originalId")
            .arg(original_id.unwrap_or(""))
            .arg("source")
            .arg(&event.source)
            .arg("eventType")
            .arg(&event.event_type)
            .query_async(&mut conn)
            .await
            .map_err(|e| {
                error!(error = %e, "Failed to add event to DLQ");
                DlqError::RedisError(e.to_string())
            })?;

        info!(
            dlq_id = %id,
            original_id = ?original_id,
            event_type = %event.event_type,
            retry_count = retry_count,
            "Event moved to Dead Letter Queue"
        );

        Ok(id)
    }

    /// Get the count of events in the DLQ.
    pub async fn count(&self) -> Result<u64, DlqError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| DlqError::ConnectionError(e.to_string()))?;

        let count: u64 = cmd("XLEN")
            .arg(DLQ_STREAM_NAME)
            .query_async(&mut conn)
            .await
            .map_err(|e| DlqError::RedisError(e.to_string()))?;

        Ok(count)
    }

    /// List events in the DLQ.
    ///
    /// Returns a list of (stream_id, failed_event) tuples.
    pub async fn list(
        &self,
        count: usize,
        offset: usize,
    ) -> Result<Vec<(String, Value)>, DlqError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| DlqError::ConnectionError(e.to_string()))?;

        // Use XRANGE to get entries
        let entries: Vec<(String, Vec<(String, String)>)> = cmd("XRANGE")
            .arg(DLQ_STREAM_NAME)
            .arg("-")
            .arg("+")
            .arg("COUNT")
            .arg(count + offset)
            .query_async(&mut conn)
            .await
            .map_err(|e| DlqError::RedisError(e.to_string()))?;

        let result: Vec<(String, Value)> = entries
            .into_iter()
            .skip(offset)
            .take(count)
            .map(|(id, fields)| {
                let mut obj = serde_json::Map::new();
                for (key, value) in fields {
                    if key == "event" {
                        // Try to parse as JSON
                        if let Ok(event) = serde_json::from_str::<Value>(&value) {
                            obj.insert(key, event);
                        } else {
                            obj.insert(key, Value::String(value));
                        }
                    } else if key == "retryCount" {
                        if let Ok(n) = value.parse::<u32>() {
                            obj.insert(key, json!(n));
                        } else {
                            obj.insert(key, Value::String(value));
                        }
                    } else {
                        obj.insert(key, Value::String(value));
                    }
                }
                (id, Value::Object(obj))
            })
            .collect();

        debug!(count = result.len(), "Retrieved DLQ entries");
        Ok(result)
    }

    /// Remove an event from the DLQ (after manual review or retry).
    pub async fn remove(&self, id: &str) -> Result<bool, DlqError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| DlqError::ConnectionError(e.to_string()))?;

        let removed: u64 = cmd("XDEL")
            .arg(DLQ_STREAM_NAME)
            .arg(id)
            .query_async(&mut conn)
            .await
            .map_err(|e| DlqError::RedisError(e.to_string()))?;

        if removed > 0 {
            info!(id = %id, "Removed event from DLQ");
            Ok(true)
        } else {
            debug!(id = %id, "Event not found in DLQ");
            Ok(false)
        }
    }

    /// Get a specific event from the DLQ by ID.
    pub async fn get(&self, id: &str) -> Result<Option<Value>, DlqError> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| DlqError::ConnectionError(e.to_string()))?;

        let entries: Vec<(String, Vec<(String, String)>)> = cmd("XRANGE")
            .arg(DLQ_STREAM_NAME)
            .arg(id)
            .arg(id)
            .query_async(&mut conn)
            .await
            .map_err(|e| DlqError::RedisError(e.to_string()))?;

        if entries.is_empty() {
            return Ok(None);
        }

        let (_, fields) = &entries[0];
        let mut obj = serde_json::Map::new();
        for (key, value) in fields {
            if key == "event" {
                if let Ok(event) = serde_json::from_str::<Value>(value) {
                    obj.insert(key.clone(), event);
                } else {
                    obj.insert(key.clone(), Value::String(value.clone()));
                }
            } else if key == "retryCount" {
                if let Ok(n) = value.parse::<u32>() {
                    obj.insert(key.clone(), json!(n));
                } else {
                    obj.insert(key.clone(), Value::String(value.clone()));
                }
            } else {
                obj.insert(key.clone(), Value::String(value.clone()));
            }
        }

        Ok(Some(Value::Object(obj)))
    }
}

/// Errors that can occur when working with the Dead Letter Queue.
#[derive(Debug, thiserror::Error)]
pub enum DlqError {
    #[error("Redis connection error: {0}")]
    ConnectionError(String),

    #[error("Redis command error: {0}")]
    RedisError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dlq_stream_name() {
        assert_eq!(DLQ_STREAM_NAME, "synapse:events:dlq");
    }

    #[test]
    fn test_failed_event_serialization() {
        let event = Event::new("test", "test.event", json!({"foo": "bar"}));
        let failed = FailedEvent {
            event,
            error: "Connection timeout".to_string(),
            retry_count: 3,
            failed_at: "2025-12-11T12:00:00Z".to_string(),
            original_id: Some("1234567890-0".to_string()),
        };

        let json = serde_json::to_string(&failed).unwrap();
        assert!(json.contains("Connection timeout"));
        assert!(json.contains("test.event"));
    }
}
