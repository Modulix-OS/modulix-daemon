//! Install/uninstall a plugin for a Modulix module.

use async_trait::async_trait;

use super::Command;
use crate::error::Error;

/// Install a plugin for a module, given the module and plugin names.
pub struct InstallPlugin;

#[async_trait]
impl Command for InstallPlugin {
    fn name(&self) -> &'static str {
        "InstallPlugin"
    }

    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let [module, plugin] = arguments else {
            unreachable!("InstallPlugin takes exactly two arguments")
        };

        tracing::info!(module = %module, plugin = %plugin, "installing module plugin");

        #[cfg(not(debug_assertions))]
        println!("install-plugin {module} {plugin}");

        Ok(format!("plugin {plugin} installed for module {module}"))
    }
}

/// Uninstall a plugin from a module, given the module and plugin names.
pub struct UninstallPlugin;

#[async_trait]
impl Command for UninstallPlugin {
    fn name(&self) -> &'static str {
        "UninstallPlugin"
    }

    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let [module, plugin] = arguments else {
            unreachable!("UninstallPlugin takes exactly two arguments")
        };

        tracing::info!(module = %module, plugin = %plugin, "uninstalling module plugin");

        #[cfg(not(debug_assertions))]
        println!("uninstall-plugin {module} {plugin}");

        Ok(format!("plugin {plugin} uninstalled for module {module}"))
    }
}

#[cfg(test)]
#[path = "plugin-tests.rs"]
mod tests;
