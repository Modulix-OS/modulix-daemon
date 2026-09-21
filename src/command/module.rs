//! Install/uninstall a Modulix module by name.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module). A meta-module (e.g.
//! `programs.games`) is expanded to its sub-modules inside core-utils, so the
//! daemon still forwards only the name(s) it received.
//!
//! Unlike `super::package`, each name in `arguments` is applied through
//! its **own** call to [`modulix_core_utils::install_module::install`]/
//! [`modulix_core_utils::install_module::uninstall`], run one after another
//! in a loop - i.e. **one transaction (and one `nixos-rebuild switch`) per
//! name**, not one transaction for the whole list. The `is_dry_run` check is
//! made once before the loop, so either every name in the call goes through
//! the library or none do; when it does, [`InstallModule::execute`]/
//! [`UninstallModule::execute`] blocks for the combined duration of every
//! per-name rebuild in sequence. Consequently, if the call for the Nth name
//! fails, the names processed before it have already been committed and
//! rebuilt (their changes are live on the system) while the names after it
//! are never attempted - a partial, non-rolled-back result, even though
//! `execute` itself still returns an error for the whole call.

use async_trait::async_trait;

use super::Command;
use crate::error::Error;

/// Installs one or more Modulix modules by name; see the module docs for the
/// per-name transaction/dry-run/blocking behaviour.
pub struct InstallModule;

#[async_trait]
impl Command for InstallModule {
    /// # Returns
    /// `"InstallModule"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "InstallModule"
    }

    /// Enables every module named in `arguments`, one
    /// [`modulix_core_utils::install_module::install`] transaction per name.
    ///
    /// # Parameters
    /// * `arguments` - one module name per element (e.g.
    ///   `"programs.games.steam"`, or a meta-module name such as
    ///   `"programs.games"`, expanded to its children inside core-utils).
    ///
    /// # Post-conditions
    /// Skipped entirely - only logged - when [`crate::dry_run::is_dry_run`]
    /// is true. Otherwise each name is installed via its own transaction and
    /// `nixos-rebuild switch`, in order; the call blocks for the combined
    /// duration of all of them. See the module docs for the consequence of a
    /// failure partway through the list (earlier names stay applied, later
    /// ones are never attempted).
    ///
    /// # Returns
    /// `"module {names} installed"`, with `names` the comma-joined
    /// `arguments` - built once up front, so it lists every requested name
    /// even if only a prefix of them actually got applied before an error.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if
    /// [`modulix_core_utils::install_module::install`] fails for any name;
    /// the loop stops at the first failure.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let names = arguments.join(", ");
        tracing::info!(names = %names, "installing module");

        if !crate::dry_run::is_dry_run() {
            for name in arguments {
                modulix_core_utils::install_module::install(crate::config_dir::config_dir(), name)
                    .await
                    .map_err(|e| Error::CoreUtils(e.to_string()))?;
            }
        }

        Ok(format!("module {names} installed"))
    }
}

/// Uninstalls one or more Modulix modules by name; see the module docs for
/// the per-name transaction/dry-run/blocking behaviour.
pub struct UninstallModule;

#[async_trait]
impl Command for UninstallModule {
    /// # Returns
    /// `"UninstallModule"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "UninstallModule"
    }

    /// Disables every module named in `arguments`, one
    /// [`modulix_core_utils::install_module::uninstall`] transaction per
    /// name.
    ///
    /// # Parameters
    /// * `arguments` - one module name per element, same layout as
    ///   [`InstallModule::execute`].
    ///
    /// # Post-conditions
    /// Skipped entirely - only logged - when [`crate::dry_run::is_dry_run`]
    /// is true. Otherwise each name is uninstalled via its own transaction
    /// and `nixos-rebuild switch`, in order; the call blocks for the
    /// combined duration of all of them. See the module docs for the
    /// consequence of a failure partway through the list.
    ///
    /// # Returns
    /// `"module {names} uninstalled"`, with `names` the comma-joined
    /// `arguments`, built up front regardless of how many names actually
    /// completed.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if
    /// [`modulix_core_utils::install_module::uninstall`] fails for any name;
    /// the loop stops at the first failure.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let names = arguments.join(", ");
        tracing::info!(names = %names, "uninstalling module");

        if !crate::dry_run::is_dry_run() {
            for name in arguments {
                modulix_core_utils::install_module::uninstall(
                    crate::config_dir::config_dir(),
                    name,
                )
                .await
                .map_err(|e| Error::CoreUtils(e.to_string()))?;
            }
        }

        Ok(format!("module {names} uninstalled"))
    }
}

#[cfg(test)]
#[path = "module-tests.rs"]
mod tests;
