//! Install/uninstall a Modulix module by name.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module). A meta-module (e.g.
//! `programs.games`) is expanded to its sub-modules inside core-utils, so the
//! daemon still forwards only the name(s) it received.

use async_trait::async_trait;

use super::Command;
use crate::error::Error;

pub struct InstallModule;

#[async_trait]
impl Command for InstallModule {
    fn name(&self) -> &'static str {
        "InstallModule"
    }

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

pub struct UninstallModule;

#[async_trait]
impl Command for UninstallModule {
    fn name(&self) -> &'static str {
        "UninstallModule"
    }

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
