//! Listened D-Bus interfaces.
//!
//! A [`Listener`] subscribes to signals/calls on an existing D-Bus interface
//! (e.g. UDisks2) and reacts to them by calling into the user's external
//! library. Each interface to watch gets its own module implementing this
//! trait; [`registry`] lists every implementation that should run.

use async_trait::async_trait;
use zbus::Connection;

use crate::error::Error;

/// A single listened D-Bus interface.
#[async_trait]
pub trait Listener: Send + Sync {
    /// Human-readable name, used in logs.
    fn name(&self) -> &'static str;

    /// Subscribe to the interface and react to its events until the
    /// connection is closed or an unrecoverable error occurs.
    async fn listen(&self, connection: Connection) -> Result<(), Error>;
}

/// All listened interfaces.
///
/// Add a new implementation here to start watching another D-Bus interface.
pub fn registry() -> Vec<Box<dyn Listener>> {
    vec![]
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
