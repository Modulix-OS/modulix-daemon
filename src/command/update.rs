//! Updates the system (`org.modulix.Daemon.UpdateSystem`).
//!
//! Unlike the other commands in this module, `UpdateSystem` is not a
//! by-name install/uninstall pair: it takes a single argument, the mode, and
//! that mode alone decides whether the running system is replaced.
//!
//! # The two modes
//!
//! * `"switch"` — the manual apply. Refreshes every flake input and
//!   **activates the result immediately** (`nixos-rebuild switch`), so no
//!   reboot is needed. This is what a user who clicked "Update Now" asked
//!   for, and the only mode that touches the running system.
//! * `"boot"` — the deferred apply. Same refresh and same rebuild, but
//!   `nixos-rebuild boot`: the running system is left alone and the new one
//!   becomes the next boot's default. This is the mode an unattended update
//!   job sends.
//!
//! `"switch"` answers `"system updated"`, `"boot"` answers `"system update
//! prepared for next boot"`.
//!
//! There is no third mode, and in particular no "download now, apply later".
//! `modulix_core_utils::update::update` is the only entry point the library
//! still exposes, and it refreshes `flake.lock` inside its own commit: a mode
//! that built without activating would commit that refreshed lock, and the
//! apply that followed would find nothing left to move and activate nothing.
//! The download and build cost is therefore paid by whichever of the two
//! modes runs. Both are capped to [`background_cores`] so the session in
//! progress keeps its share of the machine.
//!
//! # What the reply does not say
//!
//! `update` reports `Ok(())` whether it rebuilt or found every input already
//! current — the commit and the rebuild only happen if `flake.lock` moved,
//! but nothing surfaces which of the two it was. So the mode's success
//! message is returned either way, and a client that needs the distinction
//! asks `Store1.ListOutdatedInputs` again, or `Store1.RebootRequired` after a
//! `"switch"`.
//!
//! # The daemon survives its own `"switch"`
//!
//! `switch-to-configuration` restarts a unit whose definition changed, which
//! for this daemon would mean being killed in the middle of the very
//! transaction it is running — the failure class `CLAUDE.md`'s "Crash safety"
//! section describes. Two things keep that from happening: the unit sets
//! `restartIfChanged = false` and `stopIfChanged = false` (`module.nix`), so
//! the activation leaves it alone; and nixpkgs gives `dbus.service`
//! `reloadIfChanged = true`, so the bus is reloaded rather than restarted and
//! the daemon never loses its connection. `polkit` and `udisks2` *are*
//! restarted, which is harmless here: [`crate::polkit`] builds its proxy per
//! call, and the UDisks2 listeners' match rules live on the bus rather than on
//! a connection to `udisksd`.
//!
//! The consequence a client must not assume away: when the update carries a
//! new daemon, the **old** binary keeps serving until the next boot or an
//! explicit `systemctl restart`.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module), same as every other command
//! in this module: the caller still gets the mode's success reply and nothing
//! runs.

use async_trait::async_trait;
use modulix_core_utils::update::{self, BuildCommand};

use super::Command;
use crate::error::Error;

/// Halves `total`, rounding down, never below `1`.
///
/// # Parameters
/// * `total` - number of CPU cores available to the process.
///
/// # Returns
/// `(total / 2).max(1)` - a rebuild always gets at least one core, even on a
/// single-core machine.
fn half_cores(total: u32) -> u32 {
    (total / 2).max(1)
}

/// Number of CPU cores available to this process.
///
/// # Returns
/// [`std::thread::available_parallelism`]'s count, or `1` when the platform
/// cannot report it.
fn available_cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

/// Core cap for a system rebuild.
///
/// # Returns
/// `Some(half)` of the available cores. A system update rebuilds the whole
/// closure, which would otherwise starve the session it runs under — including
/// the one that asked for it.
pub(crate) fn background_cores() -> Option<u32> {
    Some(half_cores(available_cores()))
}

/// Decodes a wire mode into the activation it asks for.
///
/// This is also the mode validation: a mode this does not recognise is the
/// only thing `UpdateSystem` rejects.
///
/// # Parameters
/// * `mode` - the mode as the client sent it.
///
/// # Returns
/// `Some(BuildCommand::Switch)` for `"switch"`,
/// `Some(BuildCommand::Boot)` for `"boot"`, `None` for anything else.
fn build_command(mode: &str) -> Option<BuildCommand> {
    match mode {
        "switch" => Some(BuildCommand::Switch),
        "boot" => Some(BuildCommand::Boot),
        _ => None,
    }
}

/// Reply for `mode`, used by the dry-run path and by a successful rebuild
/// alike.
///
/// # Parameters
/// * `mode` - the validated mode string.
///
/// # Returns
/// `"system updated"` for `"switch"` - the only mode that activates the
/// running system - and `"system update prepared for next boot"` for
/// `"boot"`.
fn success_message(mode: &str) -> String {
    match mode {
        "switch" => "system updated".to_string(),
        _ => "system update prepared for next boot".to_string(),
    }
}

/// Updates the system, either activating it on demand or deferring it to the
/// next boot.
pub struct UpdateSystem;

#[async_trait]
impl Command for UpdateSystem {
    /// # Returns
    /// `"UpdateSystem"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "UpdateSystem"
    }

    /// Refreshes every flake input and rebuilds the system the way
    /// `arguments[0]` asks.
    ///
    /// # Parameters
    /// * `arguments` - exactly one element: `"switch"` or `"boot"`. See the
    ///   module docs for what each one does.
    ///
    /// # Pre-conditions
    /// The caller must hold [`crate::rebuild::guard`], which `Daemon::run`
    /// does for every command: nothing here takes it, and it is not reentrant.
    ///
    /// # Post-conditions
    /// Everything is skipped - only logged - when
    /// [`crate::dry_run::is_dry_run`] is true, and the mode's success message
    /// is returned anyway. A system whose inputs are all current writes
    /// nothing and runs no rebuild, and says so with the same message as a
    /// real update.
    ///
    /// **Only `"switch"` touches the running system**, and it replaces it
    /// outright; `"boot"` changes what the machine boots next. Blocks for the
    /// whole refresh and rebuild, potentially minutes.
    ///
    /// # Returns
    /// `"system updated"` for `"switch"`, `"system update prepared for next
    /// boot"` for `"boot"`.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if `arguments[0]` is missing or is neither mode,
    /// if the `spawn_blocking` task panics or is cancelled, or if the
    /// underlying `modulix-core-utils` call fails.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let mode = arguments
            .first()
            .copied()
            .ok_or_else(|| Error::CoreUtils("UpdateSystem: missing mode argument".to_string()))?;

        let command = build_command(mode).ok_or_else(|| {
            Error::CoreUtils(format!(
                "UpdateSystem: unknown mode '{mode}', expected 'switch' or 'boot'"
            ))
        })?;

        let cores = background_cores();
        tracing::info!(mode, ?cores, "updating system (refresh every flake input)");

        if crate::dry_run::is_dry_run() {
            tracing::info!(mode, "updating system (dry run, nothing is applied)");
            return Ok(success_message(mode));
        }

        tokio::task::spawn_blocking(move || {
            update::update(crate::config_dir::config_dir(), command, cores)
        })
        .await
        .map_err(|err| Error::CoreUtils(err.to_string()))?
        .map_err(|err| Error::CoreUtils(err.to_string()))?;

        Ok(success_message(mode))
    }
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
