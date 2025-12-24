//! WASDFX Activity Feed Effect
//!
//! Transforms Synapse events into WASDFX activity feed format and posts
//! them to the WASDFX API.
//!
//! # Example
//!
//! ```rust,ignore
//! use synapse::effects::WasdfxEffect;
//!
//! let effect = WasdfxEffect::new("http://localhost:4000/api/v1/events/emit")
//!     .with_api_key("sk_test_dev_key")
//!     .with_server_slug("haumcraft-season-vi");
//! ```

use super::{Effect, EffectError, EffectResult};
use crate::event::Event;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default timeout for WASDFX API requests
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Map Synapse event types to WASDFX ActivityType enum values
fn map_activity_type(event_type: &str) -> &'static str {
    match event_type {
        // Game events
        "game.achievement" | "player.achievement" => "ACHIEVEMENT_UNLOCKED",
        "player.joined" => "PLAYER_JOINED",
        "player.left" => "PLAYER_LEFT",
        "game.milestone" => "MILESTONE_REACHED",

        // System events
        "system.alert" | "system.announcement" => "SERVER_ANNOUNCEMENT",

        // Default fallback
        _ => "SERVER_ANNOUNCEMENT",
    }
}

/// Map event types to icon types
fn map_icon_type(event_type: &str) -> &'static str {
    if event_type.contains("achievement") {
        "achievement"
    } else if event_type.contains("joined") {
        "success"
    } else if event_type.contains("left") {
        "info"
    } else if event_type.contains("alert") || event_type.contains("warning") {
        "warning"
    } else {
        "info"
    }
}

/// Format a human-readable title from event type
fn format_title(event_type: &str, payload: &Value) -> String {
    match event_type {
        "game.achievement" | "player.achievement" => {
            let player = payload.get("player")
                .and_then(|v| v.as_str())
                .unwrap_or("Someone");
            format!("{} unlocked an achievement!", player)
        }
        "player.joined" => {
            let player = payload.get("player")
                .and_then(|v| v.as_str())
                .unwrap_or("A player");
            format!("{} joined the server", player)
        }
        "player.left" => {
            let player = payload.get("player")
                .and_then(|v| v.as_str())
                .unwrap_or("A player");
            format!("{} left the server", player)
        }
        "system.alert" => "System Alert".to_string(),
        "system.announcement" => "Server Announcement".to_string(),
        _ => {
            // Convert event.type to "Event Type"
            event_type
                .split('.')
                .map(|part| {
                    let mut chars = part.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().chain(chars).collect(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

/// Format content/description from event payload
fn format_content(event_type: &str, payload: &Value) -> String {
    match event_type {
        "game.achievement" | "player.achievement" => {
            let player = payload.get("player")
                .and_then(|v| v.as_str())
                .unwrap_or("Someone");
            let achievement = payload.get("achievement")
                .and_then(|v| v.as_str())
                .unwrap_or("an achievement");
            format!("**{}** unlocked **{}**!", player, achievement)
        }
        "player.joined" => {
            let player = payload.get("player")
                .and_then(|v| v.as_str())
                .unwrap_or("A player");
            let server = payload.get("server")
                .and_then(|v| v.as_str())
                .unwrap_or("the server");
            format!("**{}** has joined **{}**", player, server)
        }
        "player.left" => {
            let player = payload.get("player")
                .and_then(|v| v.as_str())
                .unwrap_or("A player");
            format!("**{}** has left the server", player)
        }
        _ => {
            // Try to get message field, or serialize payload
            if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                msg.to_string()
            } else {
                serde_json::to_string_pretty(payload).unwrap_or_default()
            }
        }
    }
}

/// An effect that posts events to the WASDFX Activity Feed API.
#[derive(Debug, Clone)]
pub struct WasdfxEffect {
    /// WASDFX API endpoint URL
    url: String,

    /// API key for authentication
    api_key: String,

    /// Server slug for events (e.g., "haumcraft-season-vi")
    server_slug: Option<String>,

    /// HTTP client
    client: Client,

    /// Request timeout
    timeout: Duration,
}

impl WasdfxEffect {
    /// Create a new WASDFX effect targeting the given API endpoint
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            api_key: String::new(),
            server_slug: None,
            client: Client::new(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Set the API key for authentication
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    /// Set the server slug (e.g., "haumcraft-season-vi")
    pub fn with_server_slug(mut self, server_slug: impl Into<String>) -> Self {
        self.server_slug = Some(server_slug.into());
        self
    }

    /// Set custom timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Transform a Synapse event to WASDFX activity format
    fn transform_event(&self, event: &Event) -> Value {
        let activity_type = map_activity_type(&event.event_type);
        let icon_type = map_icon_type(&event.event_type);
        let title = format_title(&event.event_type, &event.payload);
        let content = format_content(&event.event_type, &event.payload);

        // Event payload can override configured server_slug
        // This allows different event sources to specify their own server context
        let server_slug = event.payload.get("serverSlug")
            .or_else(|| event.payload.get("server_slug"))
            .and_then(|v| v.as_str())
            .map(|s| json!(s))
            .unwrap_or_else(|| json!(self.server_slug));

        // Build metadata from original event
        let metadata = json!({
            "synapse_source": event.source,
            "synapse_event_type": event.event_type,
            "synapse_correlation_id": event.correlation_id,
            "original_payload": event.payload,
        });

        json!({
            "type": activity_type,
            "title": title,
            "content": content,
            "iconType": icon_type,
            "serverSlug": server_slug,
            "category": "game",
            "priority": 0,
            "metadata": metadata,
        })
    }
}

#[async_trait]
impl Effect for WasdfxEffect {
    fn name(&self) -> &str {
        "wasdfx"
    }

    async fn execute(&self, event: &Event) -> Result<EffectResult, EffectError> {
        let payload = self.transform_event(event);

        debug!(
            url = %self.url,
            event_type = %event.event_type,
            activity_type = %payload["type"],
            "Sending to WASDFX"
        );

        let response = self.client
            .post(&self.url)
            .timeout(self.timeout)
            .header("Content-Type", "application/json")
            .header("X-API-Key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                error!(error = %e, "WASDFX request failed");
                EffectError::Http(e)
            })?;

        let status = response.status();

        if status.is_success() {
            let body: Value = response.json().await.unwrap_or(json!({}));

            info!(
                url = %self.url,
                status = %status,
                event_type = %event.event_type,
                wasdfx_event_id = %body.get("event").and_then(|e| e.get("id")).unwrap_or(&json!(null)),
                "Event posted to WASDFX"
            );

            Ok(EffectResult::with_metadata(
                self.name(),
                format!("Posted to WASDFX ({})", status),
                json!({
                    "url": self.url,
                    "status": status.as_u16(),
                    "response": body,
                }),
            ))
        } else {
            let body = response.text().await.unwrap_or_default();

            warn!(
                url = %self.url,
                status = %status,
                body = %body,
                "WASDFX request failed"
            );

            Err(EffectError::Failed(format!(
                "WASDFX returned status {}: {}",
                status, body
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_activity_type() {
        assert_eq!(map_activity_type("game.achievement"), "ACHIEVEMENT_UNLOCKED");
        assert_eq!(map_activity_type("player.joined"), "PLAYER_JOINED");
        assert_eq!(map_activity_type("player.left"), "PLAYER_LEFT");
        assert_eq!(map_activity_type("system.alert"), "SERVER_ANNOUNCEMENT");
        assert_eq!(map_activity_type("unknown.event"), "SERVER_ANNOUNCEMENT");
    }

    #[test]
    fn test_map_icon_type() {
        assert_eq!(map_icon_type("game.achievement"), "achievement");
        assert_eq!(map_icon_type("player.joined"), "success");
        assert_eq!(map_icon_type("player.left"), "info");
        assert_eq!(map_icon_type("system.alert"), "warning");
    }

    #[test]
    fn test_format_title() {
        let payload = json!({"player": "Steve", "achievement": "Diamonds"});
        assert_eq!(
            format_title("game.achievement", &payload),
            "Steve unlocked an achievement!"
        );

        let join_payload = json!({"player": "Alex"});
        assert_eq!(
            format_title("player.joined", &join_payload),
            "Alex joined the server"
        );
    }

    #[test]
    fn test_format_content() {
        let payload = json!({"player": "Steve", "achievement": "Diamonds"});
        assert_eq!(
            format_content("game.achievement", &payload),
            "**Steve** unlocked **Diamonds**!"
        );
    }

    #[test]
    fn test_transform_event() {
        let effect = WasdfxEffect::new("http://localhost:4000/api/v1/events/emit")
            .with_api_key("test-key")
            .with_server_slug("haumcraft-season-vi");

        let event = Event::new(
            "minecraft",
            "game.achievement",
            json!({"player": "Steve", "achievement": "Diamonds"})
        );

        let transformed = effect.transform_event(&event);

        assert_eq!(transformed["type"], "ACHIEVEMENT_UNLOCKED");
        assert_eq!(transformed["iconType"], "achievement");
        assert_eq!(transformed["serverSlug"], "haumcraft-season-vi");
        assert!(transformed["title"].as_str().unwrap().contains("Steve"));
    }

    #[test]
    fn test_transform_event_with_payload_server_slug() {
        let effect = WasdfxEffect::new("http://localhost:4000/api/v1/events/emit")
            .with_api_key("test-key")
            .with_server_slug("default-server");

        // Event payload overrides configured server_slug
        let event = Event::new(
            "valheim",
            "player.joined",
            json!({"player": "Viking", "serverSlug": "valhalla-server"})
        );

        let transformed = effect.transform_event(&event);

        assert_eq!(transformed["serverSlug"], "valhalla-server");
    }

    #[test]
    fn test_transform_event_with_snake_case_server_slug() {
        let effect = WasdfxEffect::new("http://localhost:4000/api/v1/events/emit")
            .with_api_key("test-key")
            .with_server_slug("default-server");

        // Also supports snake_case
        let event = Event::new(
            "satisfactory",
            "player.joined",
            json!({"player": "Engineer", "server_slug": "ficsit-factory"})
        );

        let transformed = effect.transform_event(&event);

        assert_eq!(transformed["serverSlug"], "ficsit-factory");
    }
}
