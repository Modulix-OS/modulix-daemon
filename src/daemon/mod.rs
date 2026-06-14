//! The daemon's own D-Bus interface: `org.modulix.Daemon`.
//!
//! Commands are added as methods on the [`Daemon`] interface impl below,
//! each delegating to a [`crate::command::Command`] from
//! [`crate::command::registry`].

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
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
