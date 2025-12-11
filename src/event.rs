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
///   }
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
    }

    #[test]
    fn test_event_serialize() {
        let event = Event {
            source: "minecraft".to_string(),
            event_type: "player.joined".to_string(),
            payload: json!({"player": "Steve"}),
        };

        let json_str = serde_json::to_string(&event).unwrap();
        assert!(json_str.contains("minecraft"));
        assert!(json_str.contains("eventType")); // camelCase in JSON
    }
}
