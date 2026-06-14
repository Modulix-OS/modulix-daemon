//! Install/uninstall a Modulix module by name.

use crate::lifecycle_commands;

use super::Command;
use crate::error::Error;

lifecycle_commands!(InstallModule, UninstallModule, "module");

#[cfg(test)]
#[path = "module-tests.rs"]
mod tests;
