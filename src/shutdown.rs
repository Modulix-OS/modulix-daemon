//! Graceful termination: stop taking work, let the work in flight finish.
//!
//! A configuration transaction is not interruptible. `modulix-core-utils`
//! implements neither `Drop` on its `Transaction` nor any on-disk journal, so a
//! process killed between the commit and the end of the rebuild leaves the
//! repository committed ahead of the running system, its files still sealed
//! `chattr +i`, and the caller's uncommitted work stuck in an auto-stash
//! nothing will pop. There is no resume path: the damage is only recoverable,
//! partially, by `modulix_core_utils::staging::repair_after_crash` at the next
//! start-up.
//!
//! So the daemon has to outlive its own `SIGTERM` long enough to finish. On the
//! signal it stops accepting new transactions ([`enter`] starts refusing) and
//! waits for the ones already running ([`wait_drained`]), then exits cleanly.
//!
//! This only works with the unit cooperating, and the Modulix unit does:
//! `TimeoutStopSec` is far longer than a rebuild, and `KillMode=process` means
//! the `SIGTERM` reaches this process rather than the whole control group,
//! which would take the `nixos-rebuild` down with it. Past `TimeoutStopSec`
//! systemd still sends `SIGKILL` - that is the one case the repair path exists
//! for.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::Notify;

use crate::error::Error;

/// Whether a termination signal has been seen. Once `true`, never `false`
/// again: a shutdown is not cancelled.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// How many transactions are in flight, i.e. how many [`InFlight`] tokens are
/// alive.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Woken when [`IN_FLIGHT`] reaches zero, so [`wait_drained`] does not poll.
static DRAINED: Notify = Notify::const_new();

/// Permission to run one transaction, and the proof that it is still running.
///
/// Held for as long as the transaction lasts; dropping it - including on an
/// early return or a panic - is what tells [`wait_drained`] the work is over.
/// Obtained from [`enter`] only, so a transaction that started cannot be
/// invisible to the drain.
pub struct InFlight;

impl Drop for InFlight {
    /// Marks the transaction as finished and wakes the drain when it was the
    /// last one.
    fn drop(&mut self) {
        if IN_FLIGHT.fetch_sub(1, Ordering::SeqCst) == 1 {
            DRAINED.notify_waiters();
        }
    }
}

/// Tells whether the daemon is terminating.
///
/// # Pre-conditions
/// None; callable from any thread, inside or outside a runtime.
///
/// # Returns
/// `true` once a termination signal has been received, `false` before. Never
/// goes back to `false`.
pub fn is_shutting_down() -> bool {
    SHUTTING_DOWN.load(Ordering::SeqCst)
}

/// Registers a transaction as in flight, unless the daemon is terminating.
///
/// # Pre-conditions
/// Call it before the work starts, at the outermost step of a command or a
/// listener reaction, and keep the returned token for the whole transaction.
///
/// # Post-conditions
/// On success the transaction is counted, and [`wait_drained`] will not return
/// until the token is dropped. On refusal nothing is counted and nothing must
/// be started: the process is about to exit and would be killed mid-write.
///
/// # Returns
/// `Ok(token)` while the daemon is running, `Err(Error::ShuttingDown)` once it
/// is terminating.
///
/// # Errors
/// [`Error::ShuttingDown`], the only failure mode.
pub fn enter() -> Result<InFlight, Error> {
    if is_shutting_down() {
        return Err(Error::ShuttingDown);
    }
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    let token = InFlight;

    if is_shutting_down() {
        return Err(Error::ShuttingDown);
    }
    Ok(token)
}

/// Puts the module back in its start-up state, for the tests only.
///
/// [`begin_shutdown`] is deliberately one-way, and the state is process-wide,
/// so without this one test would decide the outcome of every later one in the
/// same binary.
///
/// # Pre-conditions
/// No [`InFlight`] token may be alive.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    SHUTTING_DOWN.store(false, Ordering::SeqCst);
    IN_FLIGHT.store(0, Ordering::SeqCst);
}

/// Switches the daemon into terminating state.
///
/// # Post-conditions
/// Every later [`enter`] fails. Transactions already in flight are left alone -
/// interrupting them is the thing this module exists to avoid. Idempotent.
pub fn begin_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);
}

/// Waits until no transaction is in flight.
///
/// # Pre-conditions
/// [`begin_shutdown`] must have run first, or a new transaction can start while
/// this waits and the drain never settles.
///
/// # Post-conditions
/// Returns immediately when nothing is in flight. Does not cancel anything and
/// imposes no deadline of its own: the deadline is the unit's
/// `TimeoutStopSec`, after which systemd sends `SIGKILL`.
pub async fn wait_drained() {
    loop {
        let notified = DRAINED.notified();
        if IN_FLIGHT.load(Ordering::SeqCst) == 0 {
            return;
        }
        notified.await;
    }
}

/// Installs the `SIGTERM`/`SIGINT` handler that drains and exits.
///
/// # Pre-conditions
/// Must be called from within a Tokio runtime, once, during start-up.
///
/// # Post-conditions
/// Spawns a detached task and returns immediately. On the first signal that
/// task calls [`begin_shutdown`], waits for [`wait_drained`], then exits the
/// process with status `0` - a clean exit, because the daemon did what it was
/// asked. A second signal is not special-cased: the handler is already past
/// its `recv`, so systemd's `TimeoutStopSec` remains the way out of a rebuild
/// that never ends.
///
/// # Returns
/// `Ok(())` once both handlers are registered.
///
/// # Errors
/// [`Error::Io`] if either signal handler cannot be installed.
pub fn spawn_signal_handler() -> Result<(), Error> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;

    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => (),
            _ = interrupt.recv() => (),
        }

        begin_shutdown();
        let pending = IN_FLIGHT.load(Ordering::SeqCst);
        tracing::info!(
            pending,
            "termination signal received, draining transactions in flight"
        );

        wait_drained().await;
        tracing::info!("drained, exiting");
        std::process::exit(0);
    });

    Ok(())
}

#[cfg(test)]
#[path = "shutdown-tests.rs"]
mod tests;
