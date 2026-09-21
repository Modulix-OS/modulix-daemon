//! Install/uninstall a plugin for a Modulix module.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module). `plugin` is the bare plugin
//! name; its nixpkgs namespace (e.g. `obs-studio-plugins`) is resolved from
//! the module's own metadata (`ModuleInfo::plugins_namespace`), not passed
//! by the caller.
//!
//! [`InstallPlugin`]/[`UninstallPlugin`] serve the `InstallPlugin`/
//! `UninstallPlugin` D-Bus methods. `arguments` always holds exactly two
//! elements, in order: the module name, then the plugin name - not an
//! arbitrary name list like the package/module commands. Each call is a
//! single module + single plugin change, so it is always **one transaction**
//! with **one `nixos-rebuild switch`** (via
//! [`modulix_core_utils::install_module::install_plugin`]/
//! [`modulix_core_utils::install_module::remove_plugin`]), blocking for that
//! rebuild's whole duration. Both the plugin-namespace lookup
//! (`plugin_namespace`) and the library call itself are skipped - only
//! logged - when [`crate::dry_run::is_dry_run`] is true, so a module with no
//! resolvable plugin namespace does not surface an error in dry-run mode.

use async_trait::async_trait;
use modulix_core_utils::AppInfoMinimal;
use modulix_core_utils::module_info::ModuleInfo;

use super::Command;
use crate::error::Error;

/// Resolves the nixpkgs namespace a module's plugins are read from.
///
/// # Parameters
/// * `module` - dotted module name whose metadata is fetched.
///
/// # Returns
/// The module's plugin namespace (e.g. `"obs-studio-plugins"`), as reported
/// by its remote metadata.
///
/// # Errors
/// [`Error::CoreUtils`] if [`ModuleInfo::new`] fails to fetch/resolve the
/// module's metadata, or if the module has no plugin namespace declared
/// (`plugins_namespace()` returns `None`).
async fn plugin_namespace(module: &str) -> Result<String, Error> {
    let info = ModuleInfo::new(module)
        .await
        .map_err(|e| Error::CoreUtils(e.to_string()))?;
    info.plugins_namespace()
        .map(str::to_string)
        .ok_or_else(|| Error::CoreUtils(format!("module {module} has no plugin namespace")))
}

/// Install a plugin for a module, given the module and plugin names. See
/// the module docs for the exact D-Bus method / transaction / dry-run
/// contract shared with [`UninstallPlugin`].
pub struct InstallPlugin;

#[async_trait]
impl Command for InstallPlugin {
    /// # Returns
    /// `"InstallPlugin"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "InstallPlugin"
    }

    /// Enables `module` (if not already) and adds `plugin` to its plugin
    /// list, via [`modulix_core_utils::install_module::install_plugin`].
    ///
    /// # Parameters
    /// * `arguments` - exactly two elements: `[module, plugin]`, the module
    ///   name then the bare plugin name (its nixpkgs namespace is resolved
    ///   separately, via `plugin_namespace`).
    ///
    /// # Pre-conditions
    /// `arguments` must have exactly two elements.
    ///
    /// # Post-conditions
    /// Skipped entirely - only logged - when [`crate::dry_run::is_dry_run`]
    /// is true (this also skips the `plugin_namespace` lookup, so a module
    /// with no resolvable plugin namespace cannot fail this call in dry-run
    /// mode). Otherwise this is one transaction with one `nixos-rebuild
    /// switch`, blocking for the whole rebuild.
    ///
    /// # Returns
    /// `"plugin {plugin} installed for module {module}"`.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if `plugin_namespace` fails to resolve the
    /// module's namespace, or if
    /// [`modulix_core_utils::install_module::install_plugin`] itself fails.
    ///
    /// # Panics
    /// If `arguments` does not have exactly two elements (`unreachable!`) -
    /// callers are expected to always pass `[module, plugin]`.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let [module, plugin] = arguments else {
            unreachable!("InstallPlugin takes exactly two arguments")
        };

        tracing::info!(module = %module, plugin = %plugin, "installing module plugin");

        if !crate::dry_run::is_dry_run() {
            let namespace = plugin_namespace(module).await?;
            modulix_core_utils::install_module::install_plugin(
                crate::config_dir::config_dir(),
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

/// Uninstall a plugin from a module, given the module and plugin names. See
/// the module docs for the exact D-Bus method / transaction / dry-run
/// contract shared with [`InstallPlugin`].
pub struct UninstallPlugin;

#[async_trait]
impl Command for UninstallPlugin {
    /// # Returns
    /// `"UninstallPlugin"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "UninstallPlugin"
    }

    /// Removes `plugin` from `module`'s plugin list, via
    /// [`modulix_core_utils::install_module::remove_plugin`]. Does not
    /// change `module`'s own `enable` state.
    ///
    /// # Parameters
    /// * `arguments` - exactly two elements: `[module, plugin]`, same
    ///   layout as [`InstallPlugin::execute`].
    ///
    /// # Pre-conditions
    /// `arguments` must have exactly two elements.
    ///
    /// # Post-conditions
    /// Skipped entirely - only logged - when [`crate::dry_run::is_dry_run`]
    /// is true (this also skips the `plugin_namespace` lookup). Otherwise
    /// this is one transaction with one `nixos-rebuild switch`, blocking
    /// for the whole rebuild.
    ///
    /// # Returns
    /// `"plugin {plugin} uninstalled for module {module}"`.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if `plugin_namespace` fails to resolve the
    /// module's namespace, or if
    /// [`modulix_core_utils::install_module::remove_plugin`] itself fails.
    ///
    /// # Panics
    /// If `arguments` does not have exactly two elements (`unreachable!`) -
    /// callers are expected to always pass `[module, plugin]`.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let [module, plugin] = arguments else {
            unreachable!("UninstallPlugin takes exactly two arguments")
        };

        tracing::info!(module = %module, plugin = %plugin, "uninstalling module plugin");

        if !crate::dry_run::is_dry_run() {
            let namespace = plugin_namespace(module).await?;
            modulix_core_utils::install_module::remove_plugin(
                crate::config_dir::config_dir(),
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
