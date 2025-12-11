//! Effect system for Synapse.
//!
//! Effects are actions triggered in response to events. The [`Effect`] trait
//! defines the interface that all effect handlers must implement.
//!
//! ## Built-in Effects
//!
//! - [`LogEffect`]: Structured logging of events (useful for debugging)
//! - [`WebhookEffect`]: HTTP POST to external URLs (coming soon)
//!
//! ## Creating Custom Effects
//!
//! ```rust,ignore
//! use synapse::{Effect, EffectResult, EffectError, Event};
//! use async_trait::async_trait;
//!
//! struct MyEffect;
//!
//! #[async_trait]
//! impl Effect for MyEffect {
//!     fn name(&self) -> &str {
//!         "my-effect"
//!     }
//!
//!     async fn execute(&self, event: &Event) -> Result<EffectResult, EffectError> {
//!         // Your logic here
//!         Ok(EffectResult::success("Did the thing"))
//!     }
//! }
//! ```

pub mod log;
pub mod wasdfx;
pub mod webhook;

use crate::event::Event;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

// Re-export built-in effects
pub use log::LogEffect;
pub use wasdfx::WasdfxEffect;
pub use webhook::WebhookEffect;

/// Errors that can occur during effect execution.
#[derive(Error, Debug)]
pub enum EffectError {
    /// The effect timed out
    #[error("effect timed out after {0}ms")]
    Timeout(u64),

    /// HTTP request failed (for webhook effects)
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Serialization/deserialization error
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Generic effect failure
    #[error("effect failed: {0}")]
    Failed(String),
}

/// Result of a successful effect execution.
#[derive(Debug, Clone)]
pub struct EffectResult {
    /// Name of the effect that produced this result
    pub effect_name: String,

    /// Human-readable message describing what happened
    pub message: String,

    /// Optional metadata from the effect execution
    pub metadata: Option<serde_json::Value>,
}

impl EffectResult {
    /// Create a success result with a message
    pub fn success(effect_name: &str, message: impl Into<String>) -> Self {
        Self {
            effect_name: effect_name.to_string(),
            message: message.into(),
            metadata: None,
        }
    }

    /// Create a success result with metadata
    pub fn with_metadata(
        effect_name: &str,
        message: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Self {
        Self {
            effect_name: effect_name.to_string(),
            message: message.into(),
            metadata: Some(metadata),
        }
    }
}

/// The core Effect trait.
///
/// All effect handlers must implement this trait. Effects are async and
/// should be stateless where possible.
///
/// # Thread Safety
///
/// Effects must be `Send + Sync` to be used across async tasks.
#[async_trait]
pub trait Effect: Send + Sync {
    /// Returns the unique name of this effect (e.g., "log", "webhook", "ai-generate")
    fn name(&self) -> &str;

    /// Execute the effect for the given event.
    ///
    /// # Arguments
    ///
    /// * `event` - The event that triggered this effect
    ///
    /// # Returns
    ///
    /// * `Ok(EffectResult)` - Effect executed successfully
    /// * `Err(EffectError)` - Effect failed
    async fn execute(&self, event: &Event) -> Result<EffectResult, EffectError>;
}

/// Registry for managing effects by name.
///
/// The registry allows looking up effects by their string name, which is
/// useful for configuration-driven effect binding.
pub struct EffectRegistry {
    effects: HashMap<String, Arc<dyn Effect>>,
}

impl EffectRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            effects: HashMap::new(),
        }
    }

    /// Register an effect
    pub fn register(&mut self, effect: Arc<dyn Effect>) {
        self.effects.insert(effect.name().to_string(), effect);
    }

    /// Get an effect by name
    pub fn get(&self, name: &str) -> Option<Arc<dyn Effect>> {
        self.effects.get(name).cloned()
    }

    /// List all registered effect names
    pub fn list(&self) -> Vec<&str> {
        self.effects.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestEffect;

    #[async_trait]
    impl Effect for TestEffect {
        fn name(&self) -> &str {
            "test"
        }

        async fn execute(&self, _event: &Event) -> Result<EffectResult, EffectError> {
            Ok(EffectResult::success("test", "Test executed"))
        }
    }

    #[test]
    fn test_registry() {
        let mut registry = EffectRegistry::new();
        registry.register(Arc::new(TestEffect));

        assert!(registry.get("test").is_some());
        assert!(registry.get("nonexistent").is_none());
        assert_eq!(registry.list(), vec!["test"]);
    }

    #[test]
    fn test_effect_result() {
        let result = EffectResult::success("test", "Done");
        assert_eq!(result.effect_name, "test");
        assert_eq!(result.message, "Done");
        assert!(result.metadata.is_none());

        let result_with_meta = EffectResult::with_metadata("test", "Done", json!({"count": 42}));
        assert!(result_with_meta.metadata.is_some());
    }
}
