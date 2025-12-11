//! # Synapse Event Engine
//!
//! **W**orkers | **A**dapters | **S**ynapse | **D**ispatcher
//!
//! A high-performance, fault-tolerant engine for ingesting, routing, and
//! acting upon events from any source.
//!
//! ## Architecture
//!
//! ```text
//! Client -> HTTP API -> Redis Stream -> Worker -> Router -> Effects
//! ```
//!
//! ## Modules
//!
//! - [`event`]: Core event types shared across server and worker
//! - [`router`]: Event routing and dispatch logic
//! - [`effects`]: Effect trait and built-in effect handlers

pub mod event;
pub mod router;
pub mod effects;

// Re-export commonly used types at crate root
pub use event::Event;
pub use router::Router;
pub use effects::{Effect, EffectResult, EffectError};

/// Redis stream name for Synapse events
pub const EVENT_STREAM_NAME: &str = "synapse:events";

/// Default consumer group name
pub const DEFAULT_CONSUMER_GROUP: &str = "synapse_workers";
