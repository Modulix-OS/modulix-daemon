//! The daemon's own D-Bus interface: `org.modulix.Daemon`.
//!
//! Commands are added as methods on the [`Daemon`] interface impl below,
//! each delegating to a [`crate::command::Command`] from
//! [`crate::command::registry`].

use crate::command::setting::{Setting, apply_list, apply_option};
use crate::command::{self, Command};

/// Well-known bus name the daemon owns.
pub const BUS_NAME: &str = "org.modulix.Daemon";

/// Object path at which the [`Daemon`] interface is served.
pub const OBJECT_PATH: &str = "/org/modulix/Daemon";

/// The `org.modulix.Daemon` interface implementation.
pub struct Daemon {
    commands: Vec<Box<dyn Command>>,
}

impl Daemon {
    /// Build the interface, wiring in every command from
    /// [`crate::command::registry`].
    pub fn new() -> Self {
        Self {
            commands: command::registry(),
        }
    }

    /// Find the command named `name` and run it with `arguments`.
    async fn run(&self, name: &str, arguments: &[&str]) -> zbus::fdo::Result<String> {
        let command = self
            .commands
            .iter()
            .find(|command| command.name() == name)
            .expect("every interface method has a matching registered command");

        command.execute(arguments).await.map_err(Into::into)
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

#[zbus::interface(name = "org.modulix.Daemon")]
impl Daemon {
    /// Install a system package by name.
    async fn install_package(&self, name: &str) -> zbus::fdo::Result<String> {
        self.run("InstallPackage", &[name]).await
    }

    /// Uninstall a system package by name.
    async fn uninstall_package(&self, name: &str) -> zbus::fdo::Result<String> {
        self.run("UninstallPackage", &[name]).await
    }

    /// Install a Modulix module by name.
    async fn install_module(&self, name: &str) -> zbus::fdo::Result<String> {
        self.run("InstallModule", &[name]).await
    }

    /// Uninstall a Modulix module by name.
    async fn uninstall_module(&self, name: &str) -> zbus::fdo::Result<String> {
        self.run("UninstallModule", &[name]).await
    }

    /// Install a plugin for a module, given the module and plugin names.
    async fn install_plugin(&self, module: &str, plugin: &str) -> zbus::fdo::Result<String> {
        self.run("InstallPlugin", &[module, plugin]).await
    }

    /// Uninstall a plugin from a module, given the module and plugin names.
    async fn uninstall_plugin(&self, module: &str, plugin: &str) -> zbus::fdo::Result<String> {
        self.run("UninstallPlugin", &[module, plugin]).await
    }

    /// Apply scalar option and/or list-entry changes in a single call.
    ///
    /// Both `options` and `lists` are arrays of `(name, value, reset)`
    /// triples (D-Bus signature `a(ssb)`), so this method has the overall
    /// signature `a(ssb)a(ssb) -> s`. Either array may be empty; pass an
    /// empty array for `lists` to only change options, and vice versa.
    ///
    /// Each triple is `(name, value, reset)`:
    /// - `name`: the option/list key.
    /// - `value`: the value to set. Ignored when `reset` is `true`; pass an
    ///   empty string in that case.
    /// - `reset`: if `true`, restore `name` to its default value instead of
    ///   setting it to `value`.
    ///
    /// `options` entries are applied with the scalar-option library call;
    /// `lists` entries are applied with the dedicated list-entry library
    /// call. Entries are applied in order: all `options` first, then all
    /// `lists`.
    ///
    /// Returns one status line per entry (options first, then lists),
    /// joined with `\n`, in the same order as the input arrays.
    ///
    /// # Example (busctl)
    ///
    /// Set option `theme` to `dark`, reset option `font-size` to default,
    /// and add `vim` to list `favorites`:
    ///
    /// ```sh
    /// busctl call org.modulix.Daemon /org/modulix/Daemon org.modulix.Daemon SetOptions \
    ///   "a(ssb)" 2 "theme" "dark" false "font-size" "" true \
    ///   "a(ssb)" 1 "favorites" "vim" false
    /// ```
    async fn set_options(
        &self,
        options: Vec<Setting>,
        lists: Vec<Setting>,
    ) -> zbus::fdo::Result<String> {
        let mut results = Vec::with_capacity(options.len() + lists.len());

        for option in &options {
            results.push(apply_option(option).await?);
        }

        for list in &lists {
            results.push(apply_list(list).await?);
        }

        Ok(results.join("\n"))
    }
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
