//! Install/uninstall a plugin for a Modulix module.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module). `plugin` is the bare plugin
//! name; its nixpkgs namespace (e.g. `obs-studio-plugins`) is resolved from
//! the module's own metadata (`ModuleInfo::plugins_namespace`), not passed
//! by the caller.

use async_trait::async_trait;
use modulix_core_utils::AppInfoMinimal;
use modulix_core_utils::module_info::ModuleInfo;

use super::Command;
use crate::error::Error;

async fn plugin_namespace(module: &str) -> Result<String, Error> {
    let info = ModuleInfo::new(module)
        .await
        .map_err(|e| Error::CoreUtils(e.to_string()))?;
    info.plugins_namespace()
        .map(str::to_string)
        .ok_or_else(|| Error::CoreUtils(format!("module {module} has no plugin namespace")))
}

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

        if !crate::dry_run::is_dry_run() {
            let namespace = plugin_namespace(module).await?;
            modulix_core_utils::install_module::install_plugin(
                modulix_core_utils::CONFIG_DIRECTORY,
                module,
                &namespace,
                plugin,
            )
            .await
            .map_err(|e| Error::CoreUtils(e.to_string()))?;
        }

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

        if !crate::dry_run::is_dry_run() {
            let namespace = plugin_namespace(module).await?;
            modulix_core_utils::install_module::remove_plugin(
                modulix_core_utils::CONFIG_DIRECTORY,
                module,
                &namespace,
                plugin,
            )
            .await
            .map_err(|e| Error::CoreUtils(e.to_string()))?;
        }

        Ok(format!("plugin {plugin} uninstalled for module {module}"))
    }
}

#[cfg(test)]
#[path = "plugin-tests.rs"]
mod tests;
