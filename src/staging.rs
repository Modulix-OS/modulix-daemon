//! Daemon side of the staged-update mechanism.
//!
//! The mechanism itself lives in `modulix_core_utils::staging` (see its module
//! docs for why the candidate stays out of the git tree and why the staged copy
//! is also what gets activated). This module is the thin layer that makes it
//! fit the daemon: the dry-run gate, the info-level log before every library
//! call, and the background task that keeps a candidate warm without a client
//! having to ask.
//!
//! # Who applies a staged update
//!
//! An automatic update: nobody here. The unit `mx-apply-update.service` runs
//! `mx-apply-update` before `shutdown.target`, which is the whole point - a
//! background update never switches the running system under a live session.
//!
//! A manual update: [`apply`] with [`Activation::Switch`], reached from
//! `UpdateSystem("switch")`. A user who asked to update now asked for the new
//! system now, so that path - and only that path - replaces the closure of a
//! live session.
//!
//! [`apply`] has exactly one other caller, the administrative
//! `UpdateSystem("apply")` escape hatch, which passes [`Activation::Boot`] and
//! changes nothing but the next boot. `UpdateSystem("boot")` notably does
//! **not** reach it: it stages and leaves the promotion to the shutdown unit,
//! so no mode of this daemon writes a bootloader entry while the session runs.
//!
//! # Locking
//!
//! Neither [`stage`] nor [`apply`] takes [`crate::rebuild::guard`]: both are
//! reached from `Daemon::run`, which already holds it, and the guard is not
//! reentrant. [`spawn_background_stage`] does not go through `Daemon::run`, so
//! it takes the guard - and [`crate::shutdown::enter`] - itself.

use modulix_core_utils::staging::{self, Activation, StagedUpdate};

use crate::error::Error;

/// Resolves the available update and pre-builds it, applying nothing.
///
/// # Parameters
/// * `cores` - caps the pre-build to this many CPU cores; `None` leaves the
///   Nix default. A background staging should pass a cap, since nothing is
///   waiting on it.
///
/// # Pre-conditions
/// The caller must already hold [`crate::rebuild::guard`], and must be inside
/// a Tokio runtime.
///
/// # Post-conditions
/// Nothing is written to the configuration repository and the running system is
/// untouched. Skipped entirely - only logged - when
/// [`crate::dry_run::is_dry_run`] is true, in which case this reports `None`
/// rather than pretending something was staged.
///
/// Consumes the candidate `Store1.CheckUpdate` parked in
/// [`crate::store::take_pending_lock`] when there is one, and stages exactly
/// that lockfile: the revisions a client was shown are the revisions it gets,
/// and the minutes-long network probe is not paid twice. Consumed either way -
/// a failed staging does not put it back, the next check recomputes it. With no
/// candidate pending it runs the probe itself, so a caller never has to
/// sequence the two calls.
///
/// Blocks for the whole pre-build, plus the probe when one was needed.
///
/// # Returns
/// `Some(status)` describing what is now staged, `None` when the system is
/// already up to date or when the call was skipped by the dry-run gate.
///
/// # Errors
/// [`Error::CoreUtils`] wrapping whatever
/// [`modulix_core_utils::staging::stage_update`] reports: a failed probe, a
/// staging area that cannot be written, or a failed pre-build.
pub async fn stage(cores: Option<u32>) -> Result<Option<StagedUpdate>, Error> {
    tracing::info!(?cores, "staging a system update (resolve and pre-build)");

    if crate::dry_run::is_dry_run() {
        return Ok(None);
    }

    let config_dir = crate::config_dir::config_dir();

    match crate::store::take_pending_lock() {
        Some(candidate) => staging::stage_candidate(config_dir, candidate, cores)
            .await
            .map(Some),
        None => staging::stage_update(config_dir, cores).await,
    }
    .map_err(|err| Error::CoreUtils(err.to_string()))
}

/// Applies the staged update, activating it the way `activation` asks.
///
/// # Parameters
/// * `cores` - caps the activation's residual build; `None` leaves the Nix
///   default. A staged update is already built, so there is normally nothing
///   left to compile.
/// * `activation` - [`Activation::Boot`] to make the staged system the next
///   boot's default, [`Activation::Switch`] to activate it at once. Only a
///   user-initiated update may pass `Switch`; see the module docs.
///
/// # Pre-conditions
/// The caller must already hold [`crate::rebuild::guard`].
///
/// # Post-conditions
/// The candidate lockfile is committed either way. With [`Activation::Boot`]
/// the running system is **not** switched: the new system only becomes the
/// next boot's default. With [`Activation::Switch`] the running system has
/// already been replaced when this returns. Skipped - only logged - under
/// [`crate::dry_run::is_dry_run`], reporting `false`.
///
/// # Returns
/// `true` when a staged update was applied, `false` when there was nothing
/// usable staged (the normal case) or the call was skipped.
///
/// # Errors
/// [`Error::CoreUtils`] wrapping whatever
/// [`modulix_core_utils::staging::apply_staged_with`] reports, or a cancelled
/// blocking task.
pub async fn apply(cores: Option<u32>, activation: Activation) -> Result<bool, Error> {
    tracing::info!(?cores, ?activation, "applying the staged system update");

    if crate::dry_run::is_dry_run() {
        return Ok(false);
    }

    tokio::task::spawn_blocking(move || {
        staging::apply_staged_with(crate::config_dir::config_dir(), cores, activation)
    })
    .await
    .map_err(|err| Error::CoreUtils(err.to_string()))?
    .map_err(|err| Error::CoreUtils(err.to_string()))
}

/// Stages an update in the background, without blocking the caller.
///
/// Called at start-up and after a successful install, so a candidate is
/// resolved and built before anyone asks for it, and so the staged pre-build
/// does not stay stale behind an install that moved the configuration on.
///
/// # Pre-conditions
/// Must be called from within a Tokio runtime. The caller must **not** hold
/// [`crate::rebuild::guard`]: the spawned task takes it itself.
///
/// # Post-conditions
/// Spawns a detached task and returns immediately; the task is never joined.
/// It waits for its turn on [`crate::rebuild::guard`], so it cannot run
/// alongside a command's transaction, and registers with
/// [`crate::shutdown::enter`], so a termination signal waits for it instead of
/// killing it mid-build. A staging that fails is logged and not retried - the
/// next call is the retry.
pub fn spawn_background_stage(cores: Option<u32>) {
    tokio::spawn(async move {
        let Ok(_in_flight) = crate::shutdown::enter() else {
            return;
        };
        let _guard = crate::rebuild::guard().await;

        match stage(cores).await {
            Ok(Some(status)) => tracing::info!(
                built = status.built,
                inputs = status.inputs.len(),
                "system update staged"
            ),
            Ok(None) => tracing::info!("no system update to stage"),
            Err(err) => tracing::warn!(%err, "background staging failed"),
        }
    });
}
