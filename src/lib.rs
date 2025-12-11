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
//! - [`config`]: Configuration loading and validation
//! - [`shutdown`]: Graceful shutdown coordination

pub mod config;
pub mod dlq;
pub mod effects;
pub mod event;
pub mod router;
pub mod shutdown;

// Re-export commonly used types at crate root
pub use config::SynapseConfig;
pub use dlq::{DeadLetterQueue, DlqError, DLQ_STREAM_NAME};
pub use effects::{Effect, EffectError, EffectResult};
pub use event::Event;
pub use router::Router;
pub use shutdown::ShutdownSignal;

/// Redis stream name for Synapse events
pub const EVENT_STREAM_NAME: &str = "synapse:events";

/// Default consumer group name
pub const DEFAULT_CONSUMER_GROUP: &str = "synapse_workers";
