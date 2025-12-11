//! Webhook Effect - HTTP POST to external URLs.
//!
//! The [`WebhookEffect`] sends events to external HTTP endpoints, enabling
//! integration with external systems like Discord, Slack, or custom services.
//!
//! # Example
//!
//! ```rust,ignore
//! use synapse::effects::WebhookEffect;
//! use std::time::Duration;
//!
//! let effect = WebhookEffect::new("https://discord.com/api/webhooks/...")
//!     .with_timeout(Duration::from_secs(10))
//!     .with_retries(2)
//!     .with_discord_format();  // Use Discord embed format
//! ```

use super::{Effect, EffectError, EffectResult};
use crate::event::Event;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default timeout for webhook requests
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default number of retries on 5xx errors
const DEFAULT_RETRIES: u32 = 1;

/// Discord embed colors by event type prefix
fn get_discord_color(event_type: &str) -> u32 {
    match event_type.split('.').next().unwrap_or("") {
        "game" => 0x57F287,    // Green - positive/game events
        "player" => 0x5865F2,  // Blurple - player events
        "user" => 0x3498DB,    // Blue - user events
        "system" => 0xED4245,  // Red - system/alert events
        "error" => 0xED4245,   // Red - errors
        "warn" => 0xFEE75C,    // Yellow - warnings
        _ => 0x99AAB5,         // Gray - default
    }
}

/// Format event title for Discord
fn format_discord_title(event_type: &str) -> String {
    // Convert "game.achievement" to "Game Achievement"
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

/// An effect that sends events to an HTTP webhook endpoint.
///
/// # Features
///
/// - Configurable timeout
/// - Automatic retry on 5xx errors
/// - JSON payload with event data
/// - Discord embed format support
/// - Response status logging
#[derive(Debug, Clone)]
pub struct WebhookEffect {
    /// Target URL for the webhook
    url: String,

    /// HTTP client (reused for connection pooling)
    client: Client,

    /// Request timeout
    timeout: Duration,

    /// Number of retries on 5xx errors
    retries: u32,

    /// Use Discord embed format
    discord_format: bool,
}

impl WebhookEffect {
    /// Create a new WebhookEffect targeting the given URL
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client: Client::new(),
            timeout: DEFAULT_TIMEOUT,
            retries: DEFAULT_RETRIES,
            discord_format: false,
        }
    }

    /// Set custom timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set number of retries on 5xx errors
    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Enable Discord embed format for the webhook payload
    pub fn with_discord_format(mut self) -> Self {
        self.discord_format = true;
        self
    }

    /// Build the payload for the webhook request
    fn build_payload(&self, event: &Event) -> Value {
        if self.discord_format {
            self.build_discord_payload(event)
        } else {
            self.build_json_payload(event)
        }
    }

    /// Build standard JSON payload
    fn build_json_payload(&self, event: &Event) -> Value {
        json!({
            "source": event.source,
            "eventType": event.event_type,
            "payload": event.payload,
            "timestamp": event.timestamp,
            "correlationId": event.correlation_id,
        })
    }

    /// Build Discord embed payload
    fn build_discord_payload(&self, event: &Event) -> Value {
        let color = get_discord_color(&event.event_type);
        let title = format_discord_title(&event.event_type);

        // Build description from payload
        let description = self.format_discord_description(event);

        // Build fields from payload
        let fields = self.build_discord_fields(event);

        let mut embed = json!({
            "title": title,
            "description": description,
            "color": color,
            "footer": {
                "text": format!("Source: {}", event.source)
            }
        });

        // Add fields if we have any
        if !fields.is_empty() {
            embed["fields"] = Value::Array(fields);
        }

        // Add timestamp if available
        if let Some(ts) = &event.timestamp {
            embed["timestamp"] = json!(ts);
        }

        json!({
            "embeds": [embed]
        })
    }

    /// Format the description from event payload
    fn format_discord_description(&self, event: &Event) -> String {
        // Try to get a message field, or format the payload
        if let Some(msg) = event.payload.get("message").and_then(|v| v.as_str()) {
            return msg.to_string();
        }

        if let Some(desc) = event.payload.get("description").and_then(|v| v.as_str()) {
            return desc.to_string();
        }

        // For achievements, format nicely
        if event.event_type.contains("achievement") {
            if let (Some(player), Some(achievement)) = (
                event.payload.get("player").and_then(|v| v.as_str()),
                event.payload.get("achievement").and_then(|v| v.as_str()),
            ) {
                return format!("**{}** unlocked **{}**!", player, achievement);
            }
        }

        // For player join/leave
        if event.event_type.contains("joined") || event.event_type.contains("left") {
            if let Some(player) = event.payload.get("player").and_then(|v| v.as_str()) {
                let action = if event.event_type.contains("joined") {
                    "joined"
                } else {
                    "left"
                };
                return format!("**{}** {} the server", player, action);
            }
        }

        // Default: summarize payload
        format!("Event received from **{}**", event.source)
    }

    /// Build Discord embed fields from payload
    fn build_discord_fields(&self, event: &Event) -> Vec<Value> {
        let mut fields = Vec::new();

        if let Some(obj) = event.payload.as_object() {
            for (key, value) in obj {
                // Skip message/description as they're in the description
                if key == "message" || key == "description" {
                    continue;
                }

                // Format the value
                let field_value = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => serde_json::to_string(value).unwrap_or_default(),
                };

                // Skip empty values
                if field_value.is_empty() {
                    continue;
                }

                // Capitalize field name
                let field_name = {
                    let mut chars = key.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().chain(chars).collect(),
                    }
                };

                fields.push(json!({
                    "name": field_name,
                    "value": field_value,
                    "inline": true
                }));

                // Limit to 6 fields for clean display
                if fields.len() >= 6 {
                    break;
                }
            }
        }

        fields
    }

    /// Execute the webhook request with retries
    async fn send_request(&self, event: &Event) -> Result<reqwest::Response, EffectError> {
        let payload = self.build_payload(event);

        let mut last_error = None;
        let mut attempts = 0;

        while attempts <= self.retries {
            if attempts > 0 {
                debug!(
                    attempt = attempts,
                    max_retries = self.retries,
                    "Retrying webhook request"
                );
            }

            let result = self
                .client
                .post(&self.url)
                .timeout(self.timeout)
                .json(&payload)
                .send()
                .await;

            match result {
                Ok(response) => {
                    let status = response.status();

                    // Success
                    if status.is_success() {
                        return Ok(response);
                    }

                    // Client error - don't retry
                    if status.is_client_error() {
                        warn!(
                            status = %status,
                            url = %self.url,
                            "Webhook returned client error"
                        );
                        return Ok(response);
                    }

                    // Server error - retry if we have retries left
                    if status.is_server_error() {
                        warn!(
                            status = %status,
                            url = %self.url,
                            attempt = attempts,
                            "Webhook returned server error, will retry"
                        );
                        last_error = Some(EffectError::Failed(format!(
                            "Server error: {}",
                            status
                        )));
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        url = %self.url,
                        attempt = attempts,
                        "Webhook request failed"
                    );
                    last_error = Some(EffectError::Http(e));
                }
            }

            attempts += 1;
        }

        Err(last_error.unwrap_or_else(|| EffectError::Failed("Unknown error".into())))
    }
}

#[async_trait]
impl Effect for WebhookEffect {
    fn name(&self) -> &str {
        "webhook"
    }

    async fn execute(&self, event: &Event) -> Result<EffectResult, EffectError> {
        debug!(
            url = %self.url,
            event_type = %event.event_type,
            discord_format = self.discord_format,
            "Sending webhook"
        );

        let response = self.send_request(event).await?;
        let status = response.status();

        if status.is_success() {
            info!(
                url = %self.url,
                status = %status,
                event_type = %event.event_type,
                "Webhook delivered successfully"
            );

            Ok(EffectResult::with_metadata(
                self.name(),
                format!("Webhook delivered to {} ({})", self.url, status),
                json!({
                    "url": self.url,
                    "status": status.as_u16(),
                    "format": if self.discord_format { "discord" } else { "json" },
                }),
            ))
        } else {
            error!(
                url = %self.url,
                status = %status,
                event_type = %event.event_type,
                "Webhook delivery failed"
            );

            Err(EffectError::Failed(format!(
                "Webhook returned status {}",
                status
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_webhook_builder() {
        let effect = WebhookEffect::new("https://example.com/webhook")
            .with_timeout(Duration::from_secs(10))
            .with_retries(3);

        assert_eq!(effect.url, "https://example.com/webhook");
        assert_eq!(effect.timeout, Duration::from_secs(10));
        assert_eq!(effect.retries, 3);
        assert!(!effect.discord_format);
    }

    #[test]
    fn test_webhook_discord_format() {
        let effect = WebhookEffect::new("https://discord.com/webhook")
            .with_discord_format();

        assert!(effect.discord_format);
    }

    #[test]
    fn test_discord_color() {
        assert_eq!(get_discord_color("game.achievement"), 0x57F287);
        assert_eq!(get_discord_color("system.alert"), 0xED4245);
        assert_eq!(get_discord_color("user.created"), 0x3498DB);
        assert_eq!(get_discord_color("unknown.event"), 0x99AAB5);
    }

    #[test]
    fn test_discord_title() {
        assert_eq!(format_discord_title("game.achievement"), "Game Achievement");
        assert_eq!(format_discord_title("player.joined"), "Player Joined");
        assert_eq!(format_discord_title("system.alert"), "System Alert");
    }

    #[test]
    fn test_discord_payload_structure() {
        let effect = WebhookEffect::new("https://discord.com/webhook")
            .with_discord_format();

        let event = Event {
            source: "test".to_string(),
            event_type: "game.achievement".to_string(),
            payload: json!({
                "player": "Steve",
                "achievement": "Diamonds!"
            }),
            timestamp: Some("2025-12-11T10:00:00Z".to_string()),
            correlation_id: None,
        };

        let payload = effect.build_payload(&event);

        assert!(payload.get("embeds").is_some());
        let embed = &payload["embeds"][0];
        assert_eq!(embed["title"], "Game Achievement");
        assert_eq!(embed["color"], 0x57F287);
        assert!(embed["description"].as_str().unwrap().contains("Steve"));
    }
}
