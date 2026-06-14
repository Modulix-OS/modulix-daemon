//! Install/uninstall a system package by name.

use crate::lifecycle_commands;

use super::Command;
use crate::error::Error;

lifecycle_commands!(InstallPackage, UninstallPackage, "package");

#[cfg(test)]
#[path = "package-tests.rs"]
mod tests;
