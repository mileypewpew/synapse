//! Event Router - The brain of Synapse.
//!
//! The [`Router`] dispatches events to registered effect handlers based on
//! the event's `event_type` field, supporting both exact matches and wildcard
//! patterns.
//!
//! # Pattern Matching
//!
//! ```text
//! Pattern         | Matches
//! ----------------|---------------------------
//! user.created    | user.created (exact only)
//! game.*          | game.started, game.ended, game.achievement
//! system.*        | system.health, system.alert
//! *               | everything (catch-all)
//! ```
//!
//! # Matching Priority
//!
//! 1. Exact match handlers are checked first
//! 2. Wildcard patterns are checked in registration order
//! 3. Default handler is used if nothing matches
//!
//! # Architecture
//!
//! ```text
//! Event (type: "game.achievement")
//!     │
//!     ▼
//! ┌─────────────────────────────────────┐
//! │            ROUTER                   │
//! │                                     │
//! │  1. Exact: handlers["game.achievement"]? NO
//! │  2. Pattern: "game.*" matches? YES  │
//! │  3. Execute: LogEffect, WebhookEffect
//! └─────────────────────────────────────┘
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
//! // Exact match
//! router.on("user.created", Arc::new(LogEffect::new()));
//!
//! // Wildcard: matches game.started, game.ended, game.achievement
//! router.on("game.*", Arc::new(LogEffect::with_prefix("game")));
//!
//! // Catch-all for everything else
//! router.on("*", Arc::new(LogEffect::with_prefix("catch-all")));
//!
//! // Or use set_default for unmatched events
//! router.set_default(Arc::new(LogEffect::with_prefix("unhandled")));
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

    /// Pattern(s) that matched this event
    pub matched_patterns: Vec<String>,

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

/// A wildcard pattern and its associated effects
#[derive(Clone)]
struct PatternEntry {
    pattern: String,
    effects: Vec<Arc<dyn Effect>>,
}

/// Event router that dispatches events to registered effect handlers.
///
/// # Thread Safety
///
/// The Router is designed to be wrapped in `Arc` for shared access across
/// async tasks. Individual handlers (effects) must be `Send + Sync`.
pub struct Router {
    /// Handlers registered for exact event types
    exact_handlers: HashMap<String, Vec<Arc<dyn Effect>>>,

    /// Wildcard pattern handlers (in registration order)
    pattern_handlers: Vec<PatternEntry>,

    /// Default handler for events with no registered handlers
    default_handler: Option<Arc<dyn Effect>>,

    /// If true, stop on first effect failure. If false, continue and collect errors.
    fail_fast: bool,
}

impl Router {
    /// Create a new router with default settings.
    pub fn new() -> Self {
        Self {
            exact_handlers: HashMap::new(),
            pattern_handlers: Vec::new(),
            default_handler: None,
            fail_fast: false,
        }
    }

    /// Enable fail-fast mode: stop dispatching on first effect failure.
    pub fn with_fail_fast(mut self) -> Self {
        self.fail_fast = true;
        self
    }

    /// Register an effect handler for an event type or pattern.
    ///
    /// Multiple effects can be registered for the same pattern.
    /// They will be executed sequentially in registration order.
    ///
    /// # Patterns
    ///
    /// - Exact: `"user.created"` matches only `user.created`
    /// - Wildcard: `"game.*"` matches `game.started`, `game.ended`, etc.
    /// - Catch-all: `"*"` matches all events
    ///
    /// # Arguments
    ///
    /// * `pattern` - Event type (exact) or pattern (ending with `*`)
    /// * `effect` - The effect to execute when matched
    pub fn on(&mut self, pattern: &str, effect: Arc<dyn Effect>) {
        debug!(
            pattern = %pattern,
            effect_name = %effect.name(),
            "Registering effect handler"
        );

        if is_wildcard_pattern(pattern) {
            // Find existing pattern entry or create new one
            if let Some(entry) = self
                .pattern_handlers
                .iter_mut()
                .find(|e| e.pattern == pattern)
            {
                entry.effects.push(effect);
            } else {
                self.pattern_handlers.push(PatternEntry {
                    pattern: pattern.to_string(),
                    effects: vec![effect],
                });
            }
        } else {
            // Exact match
            self.exact_handlers
                .entry(pattern.to_string())
                .or_default()
                .push(effect);
        }
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
    ///
    /// This checks both exact matches and wildcard patterns.
    pub fn has_handlers(&self, event_type: &str) -> bool {
        if self.exact_handlers.contains_key(event_type) {
            return true;
        }

        self.pattern_handlers
            .iter()
            .any(|entry| matches_pattern(event_type, &entry.pattern))
    }

    /// Get the number of registered patterns (exact + wildcard).
    pub fn handler_count(&self) -> usize {
        self.exact_handlers.len() + self.pattern_handlers.len()
    }

    /// List all registered patterns (exact event types + wildcards).
    pub fn event_types(&self) -> Vec<&str> {
        let mut types: Vec<&str> = self.exact_handlers.keys().map(|s| s.as_str()).collect();
        for entry in &self.pattern_handlers {
            types.push(&entry.pattern);
        }
        types
    }

    /// Get all effects that should be executed for an event type.
    ///
    /// Returns (effects, matched_patterns) where matched_patterns lists
    /// which patterns matched this event.
    fn get_handlers(&self, event_type: &str) -> (Vec<Arc<dyn Effect>>, Vec<String>) {
        let mut handlers = Vec::new();
        let mut matched_patterns = Vec::new();

        // 1. Check exact match first
        if let Some(exact) = self.exact_handlers.get(event_type) {
            handlers.extend(exact.clone());
            matched_patterns.push(event_type.to_string());
        }

        // 2. Check wildcard patterns (in registration order)
        for entry in &self.pattern_handlers {
            if matches_pattern(event_type, &entry.pattern) {
                handlers.extend(entry.effects.clone());
                matched_patterns.push(entry.pattern.clone());
            }
        }

        (handlers, matched_patterns)
    }

    /// Dispatch an event to its registered handlers.
    ///
    /// # Execution Order
    ///
    /// 1. Execute handlers for exact event type match
    /// 2. Execute handlers for matching wildcard patterns
    /// 3. If no matches, execute default handler (if set) or log warning
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
            matched_patterns: Vec::new(),
            effects_executed: 0,
            results: Vec::new(),
            errors: Vec::new(),
        };

        // Get all matching handlers
        let (handlers, matched_patterns) = self.get_handlers(event_type);
        result.matched_patterns = matched_patterns;

        // If no handlers found, use default or warn
        let handlers = if handlers.is_empty() {
            if let Some(default) = &self.default_handler {
                debug!(
                    event_type = %event_type,
                    "No handlers found, using default"
                );
                result.matched_patterns.push("default".to_string());
                vec![default.clone()]
            } else {
                warn!(
                    event_type = %event_type,
                    source = %event.source,
                    "No handlers registered for event type (and no default set)"
                );
                return Ok(result);
            }
        } else {
            handlers
        };

        info!(
            event_type = %event_type,
            handler_count = handlers.len(),
            patterns = ?result.matched_patterns,
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

/// Check if a pattern is a wildcard pattern (contains `*`).
fn is_wildcard_pattern(pattern: &str) -> bool {
    pattern.contains('*')
}

/// Check if an event type matches a pattern.
///
/// # Pattern Syntax
///
/// - `"user.created"` - exact match only
/// - `"game.*"` - matches any event starting with `game.`
/// - `"*"` - matches everything (catch-all)
///
/// # Examples
///
/// ```ignore
/// assert!(matches_pattern("game.started", "game.*"));
/// assert!(matches_pattern("game.achievement", "game.*"));
/// assert!(!matches_pattern("user.created", "game.*"));
/// assert!(matches_pattern("anything", "*"));
/// ```
fn matches_pattern(event_type: &str, pattern: &str) -> bool {
    // Exact match
    if pattern == event_type {
        return true;
    }

    // Catch-all
    if pattern == "*" {
        return true;
    }

    // Wildcard suffix: "prefix.*" matches "prefix.anything"
    if let Some(prefix) = pattern.strip_suffix(".*") {
        if let Some(event_prefix) = event_type.split('.').next() {
            return event_prefix == prefix;
        }
    }

    // No match
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::LogEffect;
    use serde_json::json;

    fn test_event(event_type: &str) -> Event {
        Event::new("test", event_type, json!({"test": true}))
    }

    // Pattern matching tests

    #[test]
    fn test_matches_pattern_exact() {
        assert!(matches_pattern("user.created", "user.created"));
        assert!(!matches_pattern("user.updated", "user.created"));
    }

    #[test]
    fn test_matches_pattern_wildcard() {
        assert!(matches_pattern("game.started", "game.*"));
        assert!(matches_pattern("game.ended", "game.*"));
        assert!(matches_pattern("game.achievement", "game.*"));
        assert!(!matches_pattern("user.created", "game.*"));
        assert!(!matches_pattern("games.list", "game.*"));
    }

    #[test]
    fn test_matches_pattern_catch_all() {
        assert!(matches_pattern("anything", "*"));
        assert!(matches_pattern("user.created", "*"));
        assert!(matches_pattern("game.achievement", "*"));
        assert!(matches_pattern("", "*"));
    }

    #[test]
    fn test_is_wildcard_pattern() {
        assert!(is_wildcard_pattern("game.*"));
        assert!(is_wildcard_pattern("*"));
        assert!(!is_wildcard_pattern("user.created"));
        assert!(!is_wildcard_pattern("exact.match"));
    }

    // Router tests

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

    #[test]
    fn test_router_wildcard_registration() {
        let mut router = Router::new();

        router.on("game.*", Arc::new(LogEffect::with_prefix("game")));
        router.on("*", Arc::new(LogEffect::with_prefix("catch-all")));

        assert!(router.has_handlers("game.started"));
        assert!(router.has_handlers("game.ended"));
        assert!(router.has_handlers("anything"));
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
        assert_eq!(result.matched_patterns, vec!["test.event"]);
    }

    #[tokio::test]
    async fn test_dispatch_no_handlers() {
        let router = Router::new();
        let event = test_event("unknown.event");

        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 0);
        assert!(result.matched_patterns.is_empty());
    }

    #[tokio::test]
    async fn test_dispatch_with_default() {
        let mut router = Router::new();
        router.set_default(Arc::new(LogEffect::with_prefix("default")));

        let event = test_event("unknown.event");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 1);
        assert_eq!(result.matched_patterns, vec!["default"]);
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

    #[tokio::test]
    async fn test_dispatch_wildcard_pattern() {
        let mut router = Router::new();
        router.on("game.*", Arc::new(LogEffect::with_prefix("game")));

        let event = test_event("game.achievement");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 1);
        assert_eq!(result.matched_patterns, vec!["game.*"]);
    }

    #[tokio::test]
    async fn test_dispatch_catch_all() {
        let mut router = Router::new();
        router.on("*", Arc::new(LogEffect::with_prefix("catch-all")));

        let event = test_event("random.event");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        assert_eq!(result.effects_executed, 1);
        assert_eq!(result.matched_patterns, vec!["*"]);
    }

    #[tokio::test]
    async fn test_dispatch_exact_and_wildcard() {
        let mut router = Router::new();
        router.on("game.started", Arc::new(LogEffect::with_prefix("exact")));
        router.on("game.*", Arc::new(LogEffect::with_prefix("wildcard")));

        let event = test_event("game.started");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        // Both exact and wildcard should match
        assert_eq!(result.effects_executed, 2);
        assert!(result
            .matched_patterns
            .contains(&"game.started".to_string()));
        assert!(result.matched_patterns.contains(&"game.*".to_string()));
    }

    #[tokio::test]
    async fn test_dispatch_multiple_wildcards() {
        let mut router = Router::new();
        router.on("game.*", Arc::new(LogEffect::with_prefix("game")));
        router.on("*", Arc::new(LogEffect::with_prefix("catch-all")));

        let event = test_event("game.achievement");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        // Both wildcards should match
        assert_eq!(result.effects_executed, 2);
        assert!(result.matched_patterns.contains(&"game.*".to_string()));
        assert!(result.matched_patterns.contains(&"*".to_string()));
    }

    #[tokio::test]
    async fn test_wildcard_priority_over_default() {
        let mut router = Router::new();
        router.on("*", Arc::new(LogEffect::with_prefix("catch-all")));
        router.set_default(Arc::new(LogEffect::with_prefix("default")));

        let event = test_event("random.event");
        let result = router.dispatch(&event).await.unwrap();

        assert!(result.is_success());
        // Catch-all pattern should take priority over default handler
        assert_eq!(result.effects_executed, 1);
        assert_eq!(result.matched_patterns, vec!["*"]);
    }
}
