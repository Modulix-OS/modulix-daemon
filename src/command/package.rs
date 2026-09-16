//! Install/uninstall a system package by name.

use crate::lifecycle_commands;

use super::Command;
use crate::error::Error;

lifecycle_commands!(
    InstallPackage,
    UninstallPackage,
    "package",
    modulix_core_utils::install_package::install,
    modulix_core_utils::install_package::uninstall
);

#[cfg(test)]
#[path = "package-tests.rs"]
mod tests;
