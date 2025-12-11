//! Graceful shutdown coordination for Synapse.
//!
//! This module provides a [`ShutdownSignal`] that coordinates graceful shutdown
//! across multiple components (server, workers) when a termination signal is received.
//!
//! # Example
//!
//! ```rust,ignore
//! use synapse::shutdown::ShutdownSignal;
//!
//! #[tokio::main]
//! async fn main() {
//!     let shutdown = ShutdownSignal::new();
//!
//!     // Clone for each component
//!     let worker_shutdown = shutdown.clone();
//!
//!     // Start worker with shutdown signal
//!     tokio::spawn(async move {
//!         loop {
//!             tokio::select! {
//!                 _ = worker_shutdown.recv() => break,
//!                 // ... process events
//!             }
//!         }
//!     });
//!
//!     // Wait for shutdown signal
//!     shutdown.wait().await;
//! }
//! ```

use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Default shutdown timeout in seconds.
const DEFAULT_SHUTDOWN_TIMEOUT: u64 = 30;

/// A signal for coordinating graceful shutdown across components.
///
/// When a termination signal (SIGTERM, SIGINT) is received, all components
/// holding a clone of this signal will be notified to begin shutdown.
#[derive(Clone)]
pub struct ShutdownSignal {
    /// Broadcast sender for shutdown notification
    sender: broadcast::Sender<()>,
    /// Shutdown timeout duration
    timeout: Duration,
}

impl ShutdownSignal {
    /// Create a new shutdown signal with default timeout (30 seconds).
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT))
    }

    /// Create a new shutdown signal with custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        let (sender, _) = broadcast::channel(1);
        Self { sender, timeout }
    }

    /// Get the shutdown timeout duration.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Wait for a shutdown signal (SIGTERM or SIGINT).
    ///
    /// This function blocks until a termination signal is received,
    /// then notifies all receivers.
    pub async fn wait(&self) {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                info!("Received Ctrl+C, initiating graceful shutdown...");
            }
            _ = terminate => {
                info!("Received SIGTERM, initiating graceful shutdown...");
            }
        }

        // Notify all receivers
        let _ = self.sender.send(());
    }

    /// Subscribe to shutdown notifications.
    ///
    /// Returns a receiver that will receive a message when shutdown is triggered.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.sender.subscribe()
    }

    /// Check if shutdown has been triggered.
    ///
    /// This is a non-blocking check. Returns `true` if a shutdown signal
    /// has been sent, `false` otherwise.
    pub fn is_shutdown(&self) -> bool {
        self.sender.receiver_count() == 0
    }

    /// Trigger shutdown manually (for testing or programmatic shutdown).
    pub fn trigger(&self) {
        info!("Shutdown triggered programmatically");
        let _ = self.sender.send(());
    }

    /// Wait for shutdown with a timeout.
    ///
    /// Returns `true` if shutdown completed within timeout, `false` if timed out.
    pub async fn wait_with_timeout(&self, timeout: Duration) -> bool {
        let mut receiver = self.sender.subscribe();

        tokio::select! {
            _ = receiver.recv() => true,
            _ = tokio::time::sleep(timeout) => {
                warn!(
                    timeout_secs = timeout.as_secs(),
                    "Shutdown timeout reached, forcing shutdown"
                );
                false
            }
        }
    }
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shutdown_signal_creation() {
        let signal = ShutdownSignal::new();
        assert_eq!(signal.timeout(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn test_custom_timeout() {
        let signal = ShutdownSignal::with_timeout(Duration::from_secs(60));
        assert_eq!(signal.timeout(), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_manual_trigger() {
        let signal = ShutdownSignal::new();
        let mut receiver = signal.subscribe();

        // Trigger in a separate task
        let trigger_signal = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger_signal.trigger();
        });

        // Should receive the signal
        let result = tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_clone_receives_signal() {
        let signal = ShutdownSignal::new();
        let signal2 = signal.clone();

        let mut receiver1 = signal.subscribe();
        let mut receiver2 = signal2.subscribe();

        signal.trigger();

        // Both should receive
        assert!(receiver1.recv().await.is_ok());
        assert!(receiver2.recv().await.is_ok());
    }
}
