//! Refreshes every flake input and rebuilds the system
//! (`org.modulix.Daemon.UpdateSystem`).
//!
//! Unlike the other commands in this module, `UpdateSystem` is not a
//! by-name install/uninstall pair: it takes a single argument, the rebuild
//! mode (`"switch"` or `"boot"`), and picks how many CPU cores the rebuild's
//! `nix` build may use from that mode alone — there is no separate
//! "background" flag on the wire. `"switch"` is what the GNOME Software
//! plugin sends for a user-triggered "Update Now" (see
//! `gnome-software-plugin/CLAUDE.md`'s `update_apps_async` vfunc): the rebuild
//! is on the critical path of something the user is actively waiting on, so
//! it gets every core ([`update`] is called with `cores: None`). `"boot"` is
//! what the plugin sends when GNOME Software prepares an update in the
//! background (`GS_PLUGIN_UPDATE_APPS_FLAGS_NO_APPLY`): nothing is waiting on
//! it, so it is capped to half the machine's cores, leaving the other half
//! free for whatever the user is doing in the foreground.
//!
//! The refresh itself is *not* computed here: `Store1.CheckUpdate` already
//! resolved a candidate `flake.lock` and parked it in RAM
//! ([`crate::store::take_pending_lock`]), and this command writes that exact
//! lockfile. So the revisions the user was shown are the revisions installed,
//! and the network cost of resolving them is paid once rather than twice. A
//! call arriving with no candidate pending runs the probe itself first.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module), same as every other command
//! in this module.

use async_trait::async_trait;
use modulix_core_utils::update::BuildCommand;

use super::Command;
use crate::error::Error;

/// Halves `total`, rounding down, never below `1`.
///
/// # Parameters
/// * `total` - number of CPU cores available to the process.
///
/// # Returns
/// `(total / 2).max(1)` - a background rebuild always gets at least one
/// core, even on a single-core machine.
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

/// Refreshes every flake input and rebuilds the system.
pub struct UpdateSystem;

#[async_trait]
impl Command for UpdateSystem {
    /// # Returns
    /// `"UpdateSystem"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "UpdateSystem"
    }

    /// Applies the candidate `flake.lock` left by the last
    /// `Store1.CheckUpdate` through
    /// [`modulix_core_utils::update::update_with_lock`], with the
    /// `BuildCommand` and core cap that `arguments[0]` selects.
    ///
    /// The lockfile comes from [`crate::store::take_pending_lock`], so the
    /// system lands on exactly the revisions the check announced. When no
    /// check ran - or a previous update already consumed its result - the
    /// probe runs here instead, so a caller never has to sequence the two
    /// calls itself.
    ///
    /// # Parameters
    /// * `arguments` - exactly one element, `"switch"` or `"boot"`.
    ///
    /// # Post-conditions
    /// The library call is skipped entirely - only logged - when
    /// [`crate::dry_run::is_dry_run`] is true. A system already up to date
    /// writes nothing and runs no rebuild. Otherwise blocks for the whole
    /// rebuild (potentially minutes), preceded by the refresh probe when no
    /// candidate was pending. `"switch"` uses every core; `"boot"` is capped
    /// to [`half_cores`] of [`available_cores`] - see the module docs for why.
    /// The candidate is consumed either way: a failed rebuild does not put it
    /// back, the next check recomputes it.
    ///
    /// # Returns
    /// `"system updated (switch)"` for `"switch"`, `"system update prepared
    /// for next boot"` for `"boot"`, or `"system already up to date"` when
    /// there was nothing to apply.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if `arguments[0]` is missing or neither
    /// `"switch"` nor `"boot"`, if the refresh probe fails, if the
    /// `spawn_blocking` task panics/is cancelled, or if
    /// [`modulix_core_utils::update::update_with_lock`] itself fails.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let mode = arguments
            .first()
            .copied()
            .ok_or_else(|| Error::CoreUtils("UpdateSystem: missing mode argument".to_string()))?;

        let (build_command, cores) = match mode {
            "switch" => (BuildCommand::Switch, None),
            "boot" => (BuildCommand::Boot, Some(half_cores(available_cores()))),
            other => {
                return Err(Error::CoreUtils(format!(
                    "UpdateSystem: unknown mode '{other}', expected 'switch' or 'boot'"
                )));
            }
        };

        let lock = match crate::store::take_pending_lock() {
            Some(lock) => Some(lock),
            None => modulix_core_utils::update::check_update(crate::config_dir::config_dir())
                .await
                .map_err(|e| Error::CoreUtils(e.to_string()))?,
        };

        let Some(lock) = lock else {
            tracing::info!(mode, "system already up to date");
            return Ok("system already up to date".to_string());
        };

        tracing::info!(mode, ?cores, "updating system");

        if !crate::dry_run::is_dry_run() {
            tokio::task::spawn_blocking(move || {
                modulix_core_utils::update::update_with_lock(
                    crate::config_dir::config_dir(),
                    lock,
                    build_command,
                    cores,
                )
            })
            .await
            .map_err(|e| Error::CoreUtils(e.to_string()))?
            .map_err(|e| Error::CoreUtils(e.to_string()))?;
        }

        Ok(if mode == "switch" {
            "system updated (switch)".to_string()
        } else {
            "system update prepared for next boot".to_string()
        })
    }
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
