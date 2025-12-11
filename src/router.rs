//! Event Router - The brain of Synapse.
//!
//! The [`Router`] dispatches events to registered effect handlers based on
//! the event's `event_type` field.
//!
//! # Architecture
//!
//! ```text
//! Event (type: "user.created")
//!     │
//!     ▼
//! ┌─────────────────────────────────────┐
//! │            ROUTER                   │
//! │                                     │
//! │  handlers["user.created"] = [       │
//! │      LogEffect,                     │
//! │      WebhookEffect(discord_url),    │
//! │  ]                                  │
//! └─────────────────────────────────────┘
//!     │
//!     ▼
//! Execute effects sequentially
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use synapse::{Router, Event};
//! use synapse::effects::{LogEffect, WebhookEffect};
//! use std::sync::Arc;
//!
//! let mut router = Router::new();
//!
//! // Register handlers
//! router.on("user.created", Arc::new(LogEffect::new()));
//! router.on("user.created", Arc::new(WebhookEffect::new("https://...")));
//! router.on("game.achievement", Arc::new(LogEffect::with_prefix("game")));
//!
//! // Set default handler for unmatched events
//! router.set_default(Arc::new(LogEffect::with_prefix("unhandled")));
//!
//! // Dispatch an event
//! let results = router.dispatch(&event).await?;
//! ```

use crate::effects::{Effect, EffectError, EffectResult};
use crate::event::Event;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Errors that can occur during routing.
#[derive(Error, Debug)]
pub enum RouterError {
    /// One or more effects failed during dispatch
    #[error("effect '{effect_name}' failed: {source}")]
    EffectFailed {
        effect_name: String,
        #[source]
        source: EffectError,
    },

    /// Multiple effects failed
    #[error("{} effects failed during dispatch", .0.len())]
    MultipleFailures(Vec<RouterError>),
}

/// Result of dispatching an event through the router.
#[derive(Debug)]
pub struct DispatchResult {
    /// Event type that was dispatched
    pub event_type: String,

    /// Number of effects executed
    pub effects_executed: usize,

    /// Results from successful effect executions
    pub results: Vec<EffectResult>,

    /// Errors from failed effect executions (if fail_fast is false)
    pub errors: Vec<RouterError>,
}

impl DispatchResult {
    /// Returns true if all effects executed successfully
    pub fn is_success(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of failed effects
    pub fn failure_count(&self) -> usize {
        self.errors.len()
    }
}

/// Event router that dispatches events to registered effect handlers.
///
/// # Thread Safety
///
/// The Router is designed to be wrapped in `Arc` for shared access across
/// async tasks. Individual handlers (effects) must be `Send + Sync`.
pub struct Router {
    /// Handlers registered for specific event types
    handlers: HashMap<String, Vec<Arc<dyn Effect>>>,

    /// Default handler for events with no registered handlers
    default_handler: Option<Arc<dyn Effect>>,

    /// If true, stop on first effect failure. If false, continue and collect errors.
    fail_fast: bool,
}

impl Router {
    /// Create a new router with default settings.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            default_handler: None,
            fail_fast: false,
        }
    }

    /// Enable fail-fast mode: stop dispatching on first effect failure.
    pub fn with_fail_fast(mut self) -> Self {
        self.fail_fast = true;
        self
    }

    /// Register an effect handler for a specific event type.
    ///
    /// Multiple effects can be registered for the same event type.
    /// They will be executed sequentially in registration order.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to match (e.g., "user.created")
    /// * `effect` - The effect to execute when this event type is received
    pub fn on(&mut self, event_type: &str, effect: Arc<dyn Effect>) {
        debug!(
            event_type = %event_type,
            effect_name = %effect.name(),
            "Registering effect handler"
        );

        self.handlers
            .entry(event_type.to_string())
            .or_default()
            .push(effect);
    }

    /// Set the default handler for events with no registered handlers.
    ///
    /// If no default is set, unmatched events will be logged with a warning
    /// but not cause an error.
    pub fn set_default(&mut self, effect: Arc<dyn Effect>) {
        debug!(
            effect_name = %effect.name(),
            "Setting default handler"
        );
        self.default_handler = Some(effect);
    }

    /// Check if any handlers are registered for the given event type.
    pub fn has_handlers(&self, event_type: &str) -> bool {
        self.handlers.contains_key(event_type)
    }

    /// Get the number of registered event types.
    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// List all registered event types.
    pub fn event_types(&self) -> Vec<&str> {
        self.handlers.keys().map(|s| s.as_str()).collect()
    }

    /// Dispatch an event to its registered handlers.
    ///
    /// # Execution Order
    ///
    /// 1. Look up handlers for the event's `event_type`
    /// 2. If found, execute all handlers sequentially
    /// 3. If not found, execute default handler (if set) or log warning
    ///
    /// # Error Handling
    ///
    /// - In fail-fast mode: Returns error on first failure
    /// - In normal mode: Collects all errors and returns them in DispatchResult
    ///
    /// # Returns
    ///
    /// A `DispatchResult` containing success results and any errors.
    pub async fn dispatch(&self, event: &Event) -> Result<DispatchResult, RouterError> {
        let event_type = &event.event_type;

        debug!(
            event_type = %event_type,
            source = %event.source,
            "Dispatching event"
        );

        let mut result = DispatchResult {
            event_type: event_type.clone(),
            effects_executed: 0,
            results: Vec::new(),
            errors: Vec::new(),
        };

        // Get handlers for this event type
        let handlers = match self.handlers.get(event_type) {
            Some(h) if !h.is_empty() => h.clone(),
            _ => {
                // No handlers found - try default or warn
                if let Some(default) = &self.default_handler {
                    debug!(
                        event_type = %event_type,
                        "No handlers found, using default"
                    );
                    vec![default.clone()]
                } else {
                    warn!(
                        event_type = %event_type,
                        source = %event.source,
                        "No handlers registered for event type (and no default set)"
                    );
                    return Ok(result);
                }
            }
        };

        info!(
            event_type = %event_type,
            handler_count = handlers.len(),
            "Executing {} handler(s)",
            handlers.len()
        );

        // Execute handlers sequentially
        for effect in handlers {
            result.effects_executed += 1;

            match effect.execute(event).await {
                Ok(effect_result) => {
                    debug!(
                        effect_name = %effect.name(),
                        message = %effect_result.message,
                        "Effect executed successfully"
                    );
                    result.results.push(effect_result);
                }
                Err(e) => {
                    let error = RouterError::EffectFailed {
                        effect_name: effect.name().to_string(),
                        source: e,
                    };

                    warn!(
                        effect_name = %effect.name(),
                        error = %error,
                        "Effect execution failed"
                    );

                    if self.fail_fast {
                        return Err(error);
                    }

                    result.errors.push(error);
                }
            }
        }

        if result.errors.is_empty() {
            info!(
                event_type = %event_type,
                effects_executed = result.effects_executed,
                "Event dispatched successfully"
            );
        } else {
            warn!(
                event_type = %event_type,
                effects_executed = result.effects_executed,
                failures = result.errors.len(),
                "Event dispatched with failures"
            );
        }

        Ok(result)
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::LogEffect;
    use serde_json::json;

    fn test_event(event_type: &str) -> Event {
        Event {
            source: "test".to_string(),
            event_type: event_type.to_string(),
            payload: json!({"test": true}),
        }
    }

    #[test]
    fn test_router_registration() {
        let mut router = Router::new();

        router.on("user.created", Arc::new(LogEffect::new()));
        router.on("user.created", Arc::new(LogEffect::with_prefix("audit")));
        router.on("game.start", Arc::new(LogEffect::new()));

        assert!(router.has_handlers("user.created"));
        assert!(router.has_handlers("game.start"));
        assert!(!router.has_handlers("unknown"));
        assert_eq!(router.handler_count(), 2);
    }

    #[tokio::test]
    async fn test_dispatch_with_handlers() {
        let mut router = Router::new();
        router.on("test.event", Arc::new(LogEffect::new()));

        let event = test_event("test.event");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 1);
        assert_eq!(result.results.len(), 1);
    }

    #[tokio::test]
    async fn test_dispatch_no_handlers() {
        let router = Router::new();
        let event = test_event("unknown.event");

        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 0);
    }

    #[tokio::test]
    async fn test_dispatch_with_default() {
        let mut router = Router::new();
        router.set_default(Arc::new(LogEffect::with_prefix("default")));

        let event = test_event("unknown.event");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 1);
    }

    #[tokio::test]
    async fn test_multiple_handlers() {
        let mut router = Router::new();
        router.on("multi.event", Arc::new(LogEffect::new()));
        router.on("multi.event", Arc::new(LogEffect::with_prefix("second")));
        router.on("multi.event", Arc::new(LogEffect::with_prefix("third")));

        let event = test_event("multi.event");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 3);
        assert_eq!(result.results.len(), 3);
    }
}
