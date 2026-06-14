//! Commands exposed on the daemon's own interface (`org.modulix.Daemon`).
//!
//! Each command is a thin [`zbus`] method that delegates to a [`Command`]
//! implementation; [`registry`] lists every implementation that should be
//! wired up on [`crate::daemon::Daemon`].

use async_trait::async_trait;

use crate::error::Error;

/// A single command handler for the `org.modulix.Daemon` interface.
///
/// `dead_code` is allowed here because no command implementation exists yet;
/// once one is added and wired into [`crate::daemon::Daemon`], these methods
/// become reachable and the attribute can be removed.
#[allow(dead_code)]
#[async_trait]
pub trait Command: Send + Sync {
    /// Command name, matching the D-Bus method name.
    fn name(&self) -> &'static str;

    /// Run the command, delegating to the user's external library.
    async fn execute(&self, argument: &str) -> Result<String, Error>;
}

/// All commands exposed on `org.modulix.Daemon`.
///
/// Add a new implementation here to expose another command.
/// First planned command: install/uninstall a system package by name.
pub fn registry() -> Vec<Box<dyn Command>> {
    vec![]
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
