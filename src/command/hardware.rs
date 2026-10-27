//! Re-detect the machine's hardware and write the result to `hardware.nix`.
//!
//! Wired to [`modulix_core_utils::hardware_config::redetect_hardware_options`],
//! which probes the **live** machine (GPU slots from sysfs, laptop chassis,
//! fingerprint reader), writes the `mx.hardware.*` options it found and
//! rebuilds the system. Serves the `UpdateHardware` D-Bus method, which takes
//! no argument: there is nothing to name, the hardware is whatever is plugged
//! in.
//!
//! That call is a single configuration transaction ending in a
//! `nixos-rebuild switch`, so it blocks for the whole rebuild (can be minutes)
//! and rolls the configuration back as a whole on failure. It is skipped —
//! only logged — when [`crate::dry_run::is_dry_run`] is true.
//!
//! The detection is idempotent: a topology identical to what `hardware.nix`
//! already declares produces no commit and therefore no rebuild. core-utils
//! reports no distinction between that no-op and a rebuild that happened, so
//! the status string is the same either way.

use async_trait::async_trait;

use super::Command;
use crate::error::Error;

/// Re-detect the hardware and align the configuration with it.
///
/// Serves the `UpdateHardware` D-Bus method. The handler for a user who
/// changed a GPU — or any other detected part — and wants the configuration
/// to describe the machine again.
pub struct UpdateHardware;

#[async_trait]
impl Command for UpdateHardware {
    /// # Returns
    /// `"UpdateHardware"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "UpdateHardware"
    }

    /// Probes the machine and writes the detected `mx.hardware.*` options.
    ///
    /// # Parameters
    /// * `_arguments` - ignored. `UpdateHardware` takes no D-Bus argument;
    ///   the topology comes from the machine itself, not from the caller.
    ///
    /// # Pre-conditions
    /// The caller must hold [`crate::rebuild::guard`], which `Daemon::run`
    /// does for every command: nothing here takes it, and it is not
    /// reentrant. `lspci` should be on `PATH` — it is what names each GPU's
    /// generation. Without it every generation is left unset, which is
    /// harmless for AMD and Intel but hands a Fermi-to-Volta NVIDIA card the
    /// current driver branch instead of its legacy one.
    ///
    /// # Post-conditions
    /// On success the detected options are part of the active generation; on
    /// error `modulix-core-utils` has rolled the configuration back. Blocks
    /// for the whole `nixos-rebuild switch`, potentially minutes. Nothing is
    /// written and no rebuild is run when the detected topology already
    /// matches `hardware.nix`, nor when [`crate::dry_run::is_dry_run`] is
    /// true - the success string is returned anyway in both cases.
    ///
    /// Only the options the detection owns are touched: `mx.hardware.laptop`,
    /// `mx.hardware.has_fingerprint`, each GPU slot and the `nvidia.*BusId`
    /// values. A user's `nvidia.disable`, acceleration and compute settings
    /// are left alone.
    ///
    /// # Returns
    /// `"hardware configuration updated"`, whether the rebuild ran or the
    /// detection found nothing new.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] when the detection or the transaction fails (the
    /// configuration is then unchanged, rolled back by that library), or when
    /// the blocking task panics or is cancelled.
    async fn execute(&self, _arguments: &[&str]) -> Result<String, Error> {
        tracing::info!("re-detecting hardware and updating hardware.nix");

        if !crate::dry_run::is_dry_run() {
            tokio::task::spawn_blocking(|| {
                modulix_core_utils::hardware_config::redetect_all_hardware(
                    crate::config_dir::config_dir(),
                )
            })
            .await
            .map_err(|err| Error::CoreUtils(err.to_string()))?
            .map_err(|err| Error::CoreUtils(err.to_string()))?;
        }

        Ok("hardware configuration updated".to_string())
    }
}

#[cfg(test)]
#[path = "hardware-tests.rs"]
mod tests;
