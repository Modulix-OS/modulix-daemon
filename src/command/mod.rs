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
///
/// Implementations are looked up by [`Command::name`] in
/// [`crate::daemon::Daemon::run`] and dispatched to [`Command::execute`].
#[async_trait]
pub trait Command: Send + Sync {
    /// Command name, matching the D-Bus method name.
    ///
    /// # Returns
    /// The exact D-Bus method name this implementation serves (e.g.
    /// `"InstallPackage"`), used as the lookup key in
    /// [`crate::daemon::Daemon::run`].
    fn name(&self) -> &'static str;

    /// Run the command, delegating to the user's external library.
    ///
    /// # Parameters
    /// * `arguments` - the D-Bus method's parameters in order (e.g. one or
    ///   more package/module names, or a module name followed by a plugin
    ///   name). The library functions for package/module commands take the
    ///   whole name list at once; see each implementation's own docs for the
    ///   exact slice layout and whether the names are applied as one
    ///   transaction or one per name.
    ///
    /// # Pre-conditions
    /// The caller (`Daemon::run`) has already checked polkit authorization
    /// for this command before invoking it.
    ///
    /// # Returns
    /// A human-readable status string describing the change that was made
    /// (or would have been made, in dry-run mode), suitable for returning
    /// verbatim to the D-Bus caller.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] when the underlying `modulix-core-utils` call
    /// fails (configuration left unchanged, rolled back by that library);
    /// implementations may also surface other [`Error`] variants.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error>;
}

/// All commands exposed on `org.modulix.Daemon`.
///
/// Add a new implementation here to expose another command.
///
/// # Returns
/// One boxed [`Command`] per implementation currently wired up:
/// `package::InstallPackage`/`package::UninstallPackage`,
/// `module::InstallModule`/`module::UninstallModule`, and
/// `plugin::InstallPlugin`/`plugin::UninstallPlugin`. Consumed by
/// [`crate::daemon::Daemon::new`] to populate [`crate::daemon::Daemon`]'s
/// command table.
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
