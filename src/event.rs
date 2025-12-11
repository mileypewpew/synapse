//! Core event types for Synapse.
//!
//! The [`Event`] struct represents an event flowing through the system.
//! Events are ingested via HTTP, queued in Redis Streams, and processed
//! by workers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An event flowing through the Synapse system.
///
/// # Fields
///
/// - `source`: Origin of the event (e.g., "minecraft", "wasdfx", "iot-sensor")
/// - `event_type`: Type of event for routing (e.g., "user.created", "game.achievement")
/// - `payload`: Arbitrary JSON payload containing event-specific data
/// - `timestamp`: ISO 8601 timestamp (set by server on receipt)
/// - `correlation_id`: Optional ID for tracing event flow
///
/// # Example
///
/// ```json
/// {
///   "source": "minecraft",
///   "eventType": "player.joined",
///   "payload": {
///     "player": "Steve",
///     "server": "haumcraft"
///   },
///   "timestamp": "2025-12-11T10:00:00Z",
///   "correlationId": "abc123"
/// }
/// ```
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Event {
    /// Origin system that emitted this event
    pub source: String,

    /// Event type used for routing (e.g., "user.created", "game.achievement")
    #[serde(rename = "eventType")]
    pub event_type: String,

    /// Arbitrary JSON payload
    pub payload: Value,

    /// ISO 8601 timestamp when the event was received (set by server)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,

    /// Correlation ID for tracing event flow (from X-Correlation-ID header or generated)
    #[serde(rename = "correlationId", skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl Event {
    /// Create a new event with required fields
    pub fn new(source: impl Into<String>, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            source: source.into(),
            event_type: event_type.into(),
            payload,
            timestamp: None,
            correlation_id: None,
        }
    }

    /// Set the timestamp
    pub fn with_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// Set the correlation ID
    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }
}

/// An event with its Redis stream ID attached.
///
/// Used internally by the worker after reading from the stream.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    /// Redis stream message ID (e.g., "1234567890123-0")
    pub id: String,

    /// The event data
    pub event: Event,
}

impl StreamEvent {
    /// Create a new StreamEvent from an ID and Event
    pub fn new(id: String, event: Event) -> Self {
        Self { id, event }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_event_deserialize() {
        let json_str = r#"{
            "source": "test",
            "eventType": "user.created",
            "payload": {"user_id": 123}
        }"#;

        let event: Event = serde_json::from_str(json_str).unwrap();
        assert_eq!(event.source, "test");
        assert_eq!(event.event_type, "user.created");
        assert_eq!(event.payload["user_id"], 123);
        assert!(event.timestamp.is_none());
        assert!(event.correlation_id.is_none());
    }

    #[test]
    fn test_event_deserialize_with_metadata() {
        let json_str = r#"{
            "source": "test",
            "eventType": "user.created",
            "payload": {"user_id": 123},
            "timestamp": "2025-12-11T10:00:00Z",
            "correlationId": "abc-123"
        }"#;

        let event: Event = serde_json::from_str(json_str).unwrap();
        assert_eq!(event.timestamp, Some("2025-12-11T10:00:00Z".to_string()));
        assert_eq!(event.correlation_id, Some("abc-123".to_string()));
    }

    #[test]
    fn test_event_serialize() {
        let event = Event {
            source: "minecraft".to_string(),
            event_type: "player.joined".to_string(),
            payload: json!({"player": "Steve"}),
            timestamp: None,
            correlation_id: None,
        };

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains("minecraft"));
        assert!(json_str.contains("eventType")); // camelCase in JSON
                                                 // timestamp and correlationId should be omitted when None
        assert!(!json_str.contains("timestamp"));
        assert!(!json_str.contains("correlationId"));
    }

    #[test]
    fn test_event_serialize_with_metadata() {
        let event = Event::new("minecraft", "player.joined", json!({"player": "Steve"}))
            .with_timestamp("2025-12-11T10:00:00Z")
            .with_correlation_id("abc-123");

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains("timestamp"));
        assert!(json_str.contains("correlationId"));
    }

    #[test]
    fn test_event_builder() {
        let event = Event::new("test", "test.event", json!({}))
            .with_timestamp("2025-12-11T10:00:00Z")
            .with_correlation_id("corr-123");

        assert_eq!(event.source, "test");
        assert_eq!(event.event_type, "test.event");
        assert_eq!(event.timestamp, Some("2025-12-11T10:00:00Z".to_string()));
        assert_eq!(event.correlation_id, Some("corr-123".to_string()));
    }
}
