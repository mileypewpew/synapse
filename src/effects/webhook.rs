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
//!     .with_retries(2);
//! ```

use super::{Effect, EffectError, EffectResult};
use crate::event::Event;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Default timeout for webhook requests
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default number of retries on 5xx errors
const DEFAULT_RETRIES: u32 = 1;

/// An effect that sends events to an HTTP webhook endpoint.
///
/// # Features
///
/// - Configurable timeout
/// - Automatic retry on 5xx errors
/// - JSON payload with event data
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
}

impl WebhookEffect {
    /// Create a new WebhookEffect targeting the given URL
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client: Client::new(),
            timeout: DEFAULT_TIMEOUT,
            retries: DEFAULT_RETRIES,
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

    /// Execute the webhook request with retries
    async fn send_request(&self, event: &Event) -> Result<reqwest::Response, EffectError> {
        let payload = json!({
            "source": event.source,
            "eventType": event.event_type,
            "payload": event.payload,
        });

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
    }

    // Note: Integration tests with actual HTTP would go in tests/ directory
}
