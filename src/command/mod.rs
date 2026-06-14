//! Commands exposed on the daemon's own interface (`org.modulix.Daemon`).
//!
//! Each command is a thin [`zbus`] method that delegates to a [`Command`]
//! implementation; [`registry`] lists every implementation that should be
//! wired up on [`crate::daemon::Daemon`].

mod lifecycle;
mod module;
mod package;
mod plugin;
pub(crate) mod setting;

use async_trait::async_trait;

use crate::error::Error;
use module::{InstallModule, UninstallModule};
use package::{InstallPackage, UninstallPackage};
use plugin::{InstallPlugin, UninstallPlugin};

/// A single command handler for the `org.modulix.Daemon` interface.
#[async_trait]
pub trait Command: Send + Sync {
    /// Command name, matching the D-Bus method name.
    fn name(&self) -> &'static str;

    /// Run the command, delegating to the user's external library.
    ///
    /// `arguments` holds the D-Bus method's parameters in order (e.g. a
    /// single package/module name, or a module name followed by a plugin
    /// name).
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error>;
}

/// All commands exposed on `org.modulix.Daemon`.
///
/// Add a new implementation here to expose another command.
pub fn registry() -> Vec<Box<dyn Command>> {
    vec![
        Box::new(InstallPackage),
        Box::new(UninstallPackage),
        Box::new(InstallModule),
        Box::new(UninstallModule),
        Box::new(InstallPlugin),
        Box::new(UninstallPlugin),
    ]
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
