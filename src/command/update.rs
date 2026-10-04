//! Updates the system (`org.modulix.Daemon.UpdateSystem`).
//!
//! Unlike the other commands in this module, `UpdateSystem` is not a
//! by-name install/uninstall pair: it takes a single argument, the mode, and
//! picks how many CPU cores the build may use from that mode alone — there is
//! no separate "background" flag on the wire.
//!
//! # An update is never applied to the running system
//!
//! **No mode switches the machine.** A system update replaces the whole
//! closure, so applying one to a live session is the thing this pipeline
//! exists to avoid: the update is resolved and built up front, and the switch
//! happens at shutdown (`mx-apply-update.service` →
//! `modulix_core_utils::staging::apply_staged`), taking effect on the next
//! boot. The modes are the steps of that pipeline, and the first two map
//! one-to-one onto what GNOME Software drives (see
//! `gnome-software-plugin/CLAUDE.md`'s `update_apps_async` vfunc):
//!
//! * `"build"` — the "download" step (`GS_PLUGIN_UPDATE_APPS_FLAGS_NO_APPLY`).
//!   Resolves the candidate and realises the new closure, changing **nothing**
//!   else: the running system, the boot entries and the configuration
//!   repository are all left alone. Capped to half the cores, since nothing is
//!   waiting on it. `"stage"` is the same thing under its own name.
//! * `"boot"` — the apply step. Stages if needed, then makes the staged system
//!   the next boot's default. Half the cores, so the session in progress keeps
//!   its share of the machine.
//! * `"switch"` — accepted as a synonym of `"boot"`, for the clients that
//!   still send it. It does **not** switch: there is no in-use apply, and a
//!   client asking for one gets the deferred apply instead.
//! * `"apply"` — activates what is already staged and nothing else, for an
//!   administrator doing by hand what the shutdown unit does. Never stages, so
//!   it reports "already up to date" when there is nothing waiting.
//!
//! Every applying mode therefore answers `"system update prepared for next
//! boot"`: the machine has to be restarted to run it. That is the contract,
//! not a degraded path.
//!
//! # Where the candidate comes from
//!
//! From `modulix_core_utils::staging`, which keeps it on disk under the cache
//! directory — so it survives a daemon restart, and the pre-built closure
//! stays rooted against `nix-collect-garbage`. [`crate::staging::stage`] feeds
//! it the candidate `Store1.CheckUpdate` already resolved
//! ([`crate::store::take_pending_lock`]) rather than probing twice.
//!
//! The library call is skipped when [`crate::dry_run::is_dry_run`] is true
//! (debug builds by default; see that module), same as every other command
//! in this module: the caller still gets the mode's success reply, and nothing
//! — not even the resolution probe — actually runs.

use async_trait::async_trait;

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

/// Core cap for work nobody is waiting on.
///
/// # Returns
/// `Some(half)` of the available cores, so a background build leaves the
/// session in progress its share of the machine.
pub(crate) fn background_cores() -> Option<u32> {
    Some(half_cores(available_cores()))
}

/// Reply for `mode`, used by the dry-run path and by the modes that really did
/// apply something.
///
/// # Parameters
/// * `mode` - the validated mode string.
///
/// # Returns
/// The same human-readable status string the real call produces on success.
fn success_message(mode: &str) -> String {
    match mode {
        "build" | "stage" => "system update downloaded".to_string(),
        _ => "system update prepared for next boot".to_string(),
    }
}

/// Reply when there was nothing to do.
const ALREADY_UP_TO_DATE: &str = "system already up to date";

/// Updates the system, staging the work and deferring the switch.
pub struct UpdateSystem;

impl UpdateSystem {
    /// Resolves and pre-builds a candidate without applying it.
    ///
    /// # Returns
    /// The mode's success message, or [`ALREADY_UP_TO_DATE`] when every input
    /// is already current.
    ///
    /// # Errors
    /// As [`crate::staging::stage`].
    async fn stage_only(mode: &str) -> Result<String, Error> {
        match crate::staging::stage(background_cores()).await? {
            Some(_) => Ok(success_message(mode)),
            None => Ok(ALREADY_UP_TO_DATE.to_string()),
        }
    }

    /// Stages if needed, then makes the staged system the next boot's.
    ///
    /// # Parameters
    /// * `stage_first` - `false` for `"apply"`, which activates only what is
    ///   already staged.
    ///
    /// # Returns
    /// The `"boot"` success message, or [`ALREADY_UP_TO_DATE`] when there was
    /// nothing staged to apply.
    ///
    /// # Errors
    /// As [`crate::staging::stage`] and [`crate::staging::apply`].
    async fn stage_and_apply(stage_first: bool) -> Result<String, Error> {
        if stage_first {
            crate::staging::stage(background_cores()).await?;
        }
        if crate::staging::apply(background_cores()).await? {
            Ok(success_message("boot"))
        } else {
            Ok(ALREADY_UP_TO_DATE.to_string())
        }
    }
}

#[async_trait]
impl Command for UpdateSystem {
    /// # Returns
    /// `"UpdateSystem"` - the D-Bus method name this command serves.
    fn name(&self) -> &'static str {
        "UpdateSystem"
    }

    /// Runs the step `arguments[0]` selects, from resolving a candidate to
    /// activating it.
    ///
    /// # Parameters
    /// * `arguments` - exactly one element: `"switch"`, `"boot"`, `"build"`,
    ///   `"stage"` or `"apply"`. See the module docs for what each one does.
    ///
    /// # Pre-conditions
    /// The caller must hold [`crate::rebuild::guard`], which `Daemon::run`
    /// does for every command: nothing here takes it, and it is not reentrant.
    ///
    /// # Post-conditions
    /// Everything is skipped - only logged - when
    /// [`crate::dry_run::is_dry_run`] is true, including the resolution probe,
    /// and the mode's success message is returned anyway. A system already up
    /// to date writes nothing and runs no build.
    ///
    /// **No mode touches the running system.** `"build"`/`"stage"` change
    /// nothing at all; `"boot"`, `"switch"` and `"apply"` only change what the
    /// machine boots next, so the update takes effect on restart. The staged
    /// candidate stays on disk until it is applied or superseded. Blocks for
    /// the whole build, potentially minutes.
    ///
    /// # Returns
    /// `"system update downloaded"` for `"build"`/`"stage"`, `"system update
    /// prepared for next boot"` for `"boot"`/`"switch"`/`"apply"`, or
    /// `"system already up to date"` when there was nothing to do.
    ///
    /// # Errors
    /// [`Error::CoreUtils`] if `arguments[0]` is missing or is not one of the
    /// five modes, if the resolution probe fails, if a `spawn_blocking` task
    /// panics or is cancelled, or if the underlying `modulix-core-utils` call
    /// fails.
    async fn execute(&self, arguments: &[&str]) -> Result<String, Error> {
        let mode = arguments
            .first()
            .copied()
            .ok_or_else(|| Error::CoreUtils("UpdateSystem: missing mode argument".to_string()))?;

        if !matches!(mode, "switch" | "boot" | "build" | "stage" | "apply") {
            return Err(Error::CoreUtils(format!(
                "UpdateSystem: unknown mode '{mode}', expected 'switch', 'boot', 'build', \
                 'stage' or 'apply'"
            )));
        }

        if crate::dry_run::is_dry_run() {
            tracing::info!(mode, "updating system (dry run, nothing is applied)");
            return Ok(success_message(mode));
        }

        match mode {
            "build" | "stage" => Self::stage_only(mode).await,
            "apply" => Self::stage_and_apply(false).await,
            _ => Self::stage_and_apply(true).await,
        }
    }
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
