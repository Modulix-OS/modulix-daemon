//! Turn UEFI Secure Boot support on or off in the configuration.
//!
//! Wired to [`modulix_core_utils::secure_boot::set_secure_boot`], which
//! declares (or removes) `mx.bootloader.secureBoot.enable` in `boot.nix` and
//! rebuilds. Serves the `SetSecureBoot` D-Bus method, whose single argument is
//! the target state spelled `"true"` or `"false"`.
//!
//! Nothing here touches a key. Enabling the option is what makes mxpkgs hand
//! `autoGenerateKeys` and `autoEnrollKeys` to nixpkgs' limine module, whose
//! bootloader installer then runs `sbctl create-keys`, `sbctl enroll-keys` and
//! `sbctl sign` during the rebuild this call blocks on.
//!
//! **Enabling is refused unless the firmware is in Setup Mode.** Keys can only
//! be enrolled while no Platform Key is present, and `limine-install.py` runs
//! `sbctl` through `subprocess.run` without `check=True`, so a failed
//! enrollment is silently ignored: the rebuild would report success and leave
//! a system whose loader is unsigned and whose key databases are untouched.
//! Refusing up front is the only way the caller learns about it. Disabling is
//! never refused — dropping the option is valid in any firmware state.
//!
//! That call is a single configuration transaction ending in a
//! `nixos-rebuild switch`, so it blocks for the whole rebuild (can be minutes)
//! and rolls the configuration back as a whole on failure. It is skipped —
//! only logged — when [`crate::dry_run::is_dry_run`] is true.

use async_trait::async_trait;
use modulix_core_utils::secure_boot::{self, FirmwareState};

use super::Command;
use crate::error::Error;

/// Set Secure Boot support in the configuration.
///
/// Serves the `SetSecureBoot` D-Bus method. The handler for a user who put
/// their firmware in Setup Mode after the install and now wants Secure Boot,
/// or who wants to drop it again.
pub struct SetSecureBoot;

/// Parses the method's single argument.
///
/// # Parameters
/// * `arguments` - the D-Bus arguments; only the first is read.
///
/// # Returns
/// The requested state.
///
/// # Errors
/// [`Error::CoreUtils`] when the argument is missing or is neither `"true"`
/// nor `"false"`.
fn parse_state(arguments: &[&str]) -> Result<bool, Error> {
    let raw = arguments
        .first()
        .copied()
        .ok_or_else(|| Error::CoreUtils("SetSecureBoot: missing state argument".to_string()))?;

    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(Error::CoreUtils(format!(
            "SetSecureBoot: unknown state '{other}', expected 'true' or 'false'"
        ))),
    }
}

/// Refuses an enablement the firmware cannot honour.
///
/// # Parameters
/// * `state` - the firmware's current enrollment state.
///
/// # Returns
/// `Ok(())` when keys can be enrolled.
///
/// # Errors
/// [`Error::CoreUtils`] naming the state that blocks the enrollment and what
/// the user has to do about it.
fn require_enrollable(state: FirmwareState) -> Result<(), Error> {
    match state {
        FirmwareState::SetupMode => Ok(()),
        FirmwareState::UserMode => Err(Error::CoreUtils(
            "SetSecureBoot: the firmware already has a Platform Key enrolled, so no new \
             key set can be written. Clear the Secure Boot keys from the firmware setup \
             menu to put it back in Setup Mode, then try again."
                .to_string(),
        )),
        FirmwareState::NotUefi => Err(Error::CoreUtils(
            "SetSecureBoot: this machine did not boot in UEFI mode, so it has no Secure \
             Boot to enable."
                .to_string(),
        )),
    }
}

#[async_trait]
impl Command for SetSecureBoot {
    /// # Returns
    /// `"SetSecureBoot"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "SetSecureBoot"
    }

    /// Declares or removes the Secure Boot option and rebuilds.
    ///
    /// # Parameters
    /// * `arguments` - one element, `"true"` or `"false"`: the state to put
    ///   the option in.
    ///
    /// # Pre-conditions
    /// The caller must hold [`crate::rebuild::guard`], which `Daemon::run`
    /// does for every command: nothing here takes it, and it is not
    /// reentrant. Enabling additionally needs the firmware in Setup Mode,
    /// which is checked here and refused otherwise. Reading that state needs
    /// root on a UEFI-booted machine; the daemon runs as root.
    ///
    /// # Post-conditions
    /// On success the option is part of the active generation. Enabling means
    /// the rebuild has generated a key set, enrolled it into the firmware and
    /// signed the loader, which also takes the firmware out of Setup Mode; the
    /// user still has to switch Secure Boot on in the firmware setup menu.
    /// On error `modulix-core-utils` has rolled the configuration back. Blocks
    /// for the whole `nixos-rebuild switch`, potentially minutes. Nothing is
    /// written and no rebuild is run when [`crate::dry_run::is_dry_run`] is
    /// true - the success string is returned anyway.
    ///
    /// # Returns
    /// `"secure boot enabled"` or `"secure boot disabled"`.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] when the argument is missing or invalid, when
    /// enabling on a firmware that cannot enrol keys, when the firmware state
    /// cannot be read, when the transaction fails (the configuration is then
    /// unchanged, rolled back by that library), or when the blocking task
    /// panics or is cancelled.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let enable = parse_state(arguments)?;

        if enable {
            let state = secure_boot::firmware_state()
                .map_err(|err| Error::CoreUtils(format!("SetSecureBoot: {err}")))?;
            require_enrollable(state)?;
        }

        tracing::info!(enable, "setting secure boot in boot.nix");

        if crate::dry_run::is_dry_run() {
            tracing::info!(enable, "setting secure boot (dry run, nothing is applied)");
            return Ok(success_message(enable));
        }

        tokio::task::spawn_blocking(move || {
            secure_boot::set_secure_boot(crate::config_dir::config_dir(), enable)
        })
        .await
        .map_err(|err| Error::CoreUtils(err.to_string()))?
        .map_err(|err| Error::CoreUtils(err.to_string()))?;

        Ok(success_message(enable))
    }
}

/// Human-readable outcome of a successful call.
///
/// # Parameters
/// * `enable` - the state the option was put in.
///
/// # Returns
/// `"secure boot enabled"` or `"secure boot disabled"`.
fn success_message(enable: bool) -> String {
    if enable {
        "secure boot enabled".to_string()
    } else {
        "secure boot disabled".to_string()
    }
}

#[cfg(test)]
#[path = "secure_boot-tests.rs"]
mod tests;
