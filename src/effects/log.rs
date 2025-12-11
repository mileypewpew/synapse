//! Log Effect - Structured logging of events.
//!
//! The [`LogEffect`] provides structured logging of events using the `tracing`
//! crate. This is useful for debugging, auditing, and observability.
//!
//! # Example
//!
//! ```rust,ignore
//! use synapse::effects::LogEffect;
//!
//! let effect = LogEffect::new();
//! // or with custom prefix
//! let effect = LogEffect::with_prefix("audit");
//! ```

use super::{Effect, EffectError, EffectResult};
use crate::event::Event;
use async_trait::async_trait;
use tracing::info;

/// An effect that logs events using structured logging.
///
/// This effect is primarily useful for:
/// - Debugging event flow
/// - Audit trails
/// - Development/testing
#[derive(Debug, Clone)]
pub struct LogEffect {
    /// Optional prefix for log messages
    prefix: String,
}

impl LogEffect {
    /// Create a new LogEffect with default settings
    pub fn new() -> Self {
        Self {
            prefix: "event".to_string(),
        }
    }

    /// Create a LogEffect with a custom prefix
    ///
    /// The prefix appears in log messages, useful for distinguishing
    /// different log effects (e.g., "audit", "debug", "analytics")
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }
}

impl Default for LogEffect {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Effect for LogEffect {
    fn name(&self) -> &str {
        "log"
    }

    async fn execute(&self, event: &Event) -> Result<EffectResult, EffectError> {
        // Structured logging with tracing
        info!(
            prefix = %self.prefix,
            source = %event.source,
            event_type = %event.event_type,
            payload = %event.payload,
            "[{}] Processed: {}/{}",
            self.prefix,
            event.source,
            event.event_type
        );

        Ok(EffectResult::success(
            self.name(),
            format!(
                "Logged event {}/{} with prefix '{}'",
                event.source, event.event_type, self.prefix
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_log_effect() {
        let effect = LogEffect::new();
        let event = Event::new("test", "user.created", json!({"user_id": 123}));

        let result = effect.execute(&event).await.unwrap();
        assert_eq!(result.effect_name, "log");
        assert!(result.message.contains("test"));
        assert!(result.message.contains("user.created"));
    }

    #[tokio::test]
    async fn test_log_effect_with_prefix() {
        let effect = LogEffect::with_prefix("audit");
        let event = Event::new("admin", "user.deleted", json!({}));

        let result = effect.execute(&event).await.unwrap();
        assert!(result.message.contains("audit"));
    }
}
