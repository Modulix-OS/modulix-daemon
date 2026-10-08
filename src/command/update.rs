//! Updates the system (`org.modulix.Daemon.UpdateSystem`).
//!
//! Unlike the other commands in this module, `UpdateSystem` is not a
//! by-name install/uninstall pair: it takes a single argument, the mode, and
//! picks how many CPU cores the build may use from that mode alone — there is
//! no separate "background" flag on the wire.
//!
//! # Which mode touches the running system
//!
//! Exactly one: `"switch"`. A system update replaces the whole closure, so
//! applying one to a live session is not something to do behind a user's back
//! — but it is precisely what a user who asked to update *now* wants. The
//! split is therefore by *who asked*, not by what is technically possible:
//!
//! * an **automatic** update only ever *stages*. The `nixos-rebuild boot` that
//!   promotes the staged system is run by `mx-apply-update.service` at
//!   shutdown (`modulix_core_utils::staging::apply_staged`), so the update
//!   takes effect on the next boot and no mode of this command writes a
//!   bootloader entry while the session runs;
//! * a **manual** update goes through `"switch"` and takes effect at once.
//!
//! The modes are the steps of that pipeline, and the first two map one-to-one
//! onto what GNOME Software drives (see `gnome-software-plugin/CLAUDE.md`'s
//! `update_apps_async` vfunc):
//!
//! * `"build"` — the "download" step (`GS_PLUGIN_UPDATE_APPS_FLAGS_NO_APPLY`).
//!   Resolves the candidate and realises the new closure, changing **nothing**
//!   else: the running system, the boot entries and the configuration
//!   repository are all left alone. Capped to half the cores, since nothing is
//!   waiting on it. `"stage"` is the same thing under its own name.
//! * `"boot"` — the deferred apply. Stages, and stops there: promoting the
//!   staged system is the shutdown unit's job and is never done from here, so
//!   the *work* is the same as `"build"`'s and only the reply differs. What it
//!   adds is the promise that this is the update the next boot will carry,
//!   which is what sends GNOME Software's row to `PENDING_INSTALL`. Half the
//!   cores, same reason.
//! * `"switch"` — the manual apply. Stages if needed, then **activates the
//!   staged system immediately** (`nixos-rebuild switch`), so no reboot is
//!   needed. Half the cores, so the session in progress keeps its share of the
//!   machine.
//! * `"apply"` — makes what is already staged the next boot's system and
//!   nothing else, for an administrator doing by hand what the shutdown unit
//!   does. Never stages, so it reports "already up to date" when there is
//!   nothing waiting.
//!
//! So `"switch"` answers `"system updated"`, `"boot"` and `"apply"` answer
//! `"system update prepared for next boot"` — the machine has to be restarted
//! to run it — and `"build"`/`"stage"` answer `"system update downloaded"`.
//!
//! One consequence of leaving the promotion to the shutdown unit: a machine
//! that loses power instead of shutting down cleanly never runs it, so the
//! staged update is not promoted and the next boot carries the old system. It
//! stays staged on disk, and the next clean shutdown applies it.
//!
//! # The daemon survives its own `"switch"`
//!
//! `switch-to-configuration` restarts a unit whose definition changed, which
//! for this daemon would mean being killed in the middle of the very
//! transaction it is running — the failure class `CLAUDE.md`'s "Crash safety"
//! section describes. Two things keep that from happening: the unit sets
//! `restartIfChanged = false` and `stopIfChanged = false` (mxpkgs
//! `modulixos/modulix-daemon/default.nix`), so the activation leaves it alone;
//! and nixpkgs gives `dbus.service` `reloadIfChanged = true`, so the bus is
//! reloaded rather than restarted and the daemon never loses its connection.
//! `polkit` and `udisks2` *are* restarted, which is harmless here:
//! [`crate::polkit`] builds its proxy per call, and the UDisks2 listeners'
//! match rules live on the bus rather than on a connection to `udisksd`.
//!
//! The consequence a client must not assume away: when the update carries a
//! new daemon, the **old** binary keeps serving until the next boot or an
//! explicit `systemctl restart`.
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
use modulix_core_utils::staging::Activation;

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

/// The steps a mode runs: whether to resolve and pre-build a candidate first,
/// and how - if at all - to activate it afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Steps {
    /// Resolve and pre-build a candidate before activating.
    stage: bool,
    /// How to activate the staged system; `None` activates nothing.
    activation: Option<Activation>,
}

/// Decodes a wire mode into the steps it runs.
///
/// This is also the mode validation: a mode this does not recognise is the
/// only thing `UpdateSystem` rejects.
///
/// # Parameters
/// * `mode` - the mode as the client sent it.
///
/// # Returns
/// `Some(steps)` for `"build"`, `"stage"`, `"switch"`, `"boot"` and
/// `"apply"`; `None` for anything else.
///
/// Only `"switch"` and `"apply"` activate anything, so they are the only two
/// that reach [`crate::staging::apply`]. `"boot"` stages like `"build"` and
/// `"stage"` do and stops there - promoting the staged system is
/// `mx-apply-update.service`'s job at shutdown - so the three share an arm,
/// and what distinguishes `"boot"` is only its reply (see
/// [`success_message`]). `"apply"` is the one mode that does not stage.
fn steps(mode: &str) -> Option<Steps> {
    match mode {
        "build" | "stage" | "boot" => Some(Steps {
            stage: true,
            activation: None,
        }),
        "switch" => Some(Steps {
            stage: true,
            activation: Some(Activation::Switch),
        }),
        "apply" => Some(Steps {
            stage: false,
            activation: Some(Activation::Boot),
        }),
        _ => None,
    }
}

/// Reply for `mode`, used by the dry-run path and by the modes that really did
/// apply something.
///
/// # Parameters
/// * `mode` - the validated mode string.
///
/// # Returns
/// The same human-readable status string the real call produces on success:
/// `"system update downloaded"` for `"build"`/`"stage"`, `"system updated"`
/// for `"switch"` - the only mode that activates the running system - and
/// `"system update prepared for next boot"` for `"boot"` and `"apply"`.
///
/// `"boot"` and `"build"` do the same work (see [`steps`]) and are told apart
/// here and only here, so this is the whole difference a client observes
/// between the download job and the apply job.
fn success_message(mode: &str) -> String {
    match mode {
        "build" | "stage" => "system update downloaded".to_string(),
        "switch" => "system updated".to_string(),
        _ => "system update prepared for next boot".to_string(),
    }
}

/// Reply when there was nothing to do.
const ALREADY_UP_TO_DATE: &str = "system already up to date";

/// Updates the system, either applying it on demand or deferring it to the
/// next boot.
pub struct UpdateSystem;

impl UpdateSystem {
    /// Resolves and pre-builds a candidate without applying it.
    ///
    /// # Parameters
    /// * `mode` - the validated mode, used only to pick the reply.
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

    /// Stages if needed, then activates the staged system.
    ///
    /// # Parameters
    /// * `mode` - the validated mode, used only to pick the reply.
    /// * `stage_first` - `true` for `"switch"`; `false` for `"apply"`, which
    ///   activates only what is already staged.
    /// * `activation` - [`Activation::Switch`] for `"switch"`, which replaces
    ///   the running system; [`Activation::Boot`] for `"apply"`. These two
    ///   modes are the only callers: `"boot"` no longer activates anything
    ///   from here.
    ///
    /// # Returns
    /// The mode's success message, or [`ALREADY_UP_TO_DATE`] when there was
    /// nothing staged to apply.
    ///
    /// # Errors
    /// As [`crate::staging::stage`] and [`crate::staging::apply`].
    async fn stage_and_apply(
        mode: &str,
        stage_first: bool,
        activation: Activation,
    ) -> Result<String, Error> {
        if stage_first {
            crate::staging::stage(background_cores()).await?;
        }
        if crate::staging::apply(background_cores(), activation).await? {
            Ok(success_message(mode))
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
    /// **Only `"switch"` touches the running system**, and it replaces it
    /// outright. `"build"`, `"stage"` and `"boot"` change nothing at all -
    /// they realise the closure and leave it staged for the shutdown unit to
    /// promote; `"apply"` changes what the machine boots next. The staged
    /// candidate stays on disk until it is applied or superseded. Blocks for
    /// the whole build, potentially minutes.
    ///
    /// # Returns
    /// `"system update downloaded"` for `"build"`/`"stage"`, `"system
    /// updated"` for `"switch"`, `"system update prepared for next boot"` for
    /// `"boot"`/`"apply"`, or `"system already up to date"` when there was
    /// nothing to do.
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

        let steps = steps(mode).ok_or_else(|| {
            Error::CoreUtils(format!(
                "UpdateSystem: unknown mode '{mode}', expected 'switch', 'boot', 'build', \
                 'stage' or 'apply'"
            ))
        })?;

        if crate::dry_run::is_dry_run() {
            tracing::info!(mode, "updating system (dry run, nothing is applied)");
            return Ok(success_message(mode));
        }

        match steps.activation {
            None => Self::stage_only(mode).await,
            Some(activation) => Self::stage_and_apply(mode, steps.stage, activation).await,
        }
    }
}

#[cfg(test)]
#[path = "update-tests.rs"]
mod tests;
