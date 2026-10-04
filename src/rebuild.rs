//! One lock for every configuration transaction this process starts.
//!
//! `modulix-core-utils` serialises *rebuilds* between processes with its own
//! file-based build queue, but it serialises nothing inside one process, and
//! the rebuild is only the last step of a transaction. Everything before it -
//! the auto-stash of a dirty working tree, clearing the `chattr +i` seals,
//! writing the files, `git add`, the commit - runs unguarded on a single git
//! repository. Two of those at once on `/etc/modulix-os` interleave: one
//! transaction's stash swallows the other's uncommitted write, and the loser's
//! rollback resets `HEAD` past a commit it never made.
//!
//! The daemon reaches that code from several places at once by design: every
//! `org.modulix.Daemon` write method (install/uninstall of a package, a module
//! or a plugin, a system update) plus the UDisks2 listeners, which fire on
//! their own, whenever `/etc/fstab` changes. So they all take [`guard`] first,
//! and a burst becomes a queue instead of a race.
//!
//! The guard is held across the whole transaction, rebuild included, which is
//! minutes. That is deliberate: it is the configuration repository that is
//! being protected, and it stays inconsistent until the transaction either
//! commits or rolls back.

use tokio::sync::{Mutex, MutexGuard};

/// The lock itself. A `tokio` mutex, because it is held across `await` points
/// (the `spawn_blocking` that runs the transaction).
static REBUILD_GUARD: Mutex<()> = Mutex::const_new(());

/// Takes the configuration-transaction lock, waiting for its turn.
///
/// # Pre-conditions
/// Must be called from a Tokio runtime. The caller must not already hold the
/// guard: it is not reentrant, and a nested call deadlocks. In practice that
/// means taking it once, at the outermost step of a command or a listener
/// reaction, never inside a helper that a guarded caller also reaches.
///
/// # Post-conditions
/// Blocks until no other transaction of this process is in flight. The lock is
/// released when the returned guard is dropped, including on an early return or
/// a panic, so a failed transaction cannot leave it held. Never poisoned - a
/// `tokio` mutex has no poisoning - so a panicking transaction does not take
/// every later one down with it.
///
/// # Returns
/// The guard. It must be bound (`let _guard = …`), not dropped immediately with
/// `let _ = …`, which would release the lock before the work it protects.
pub async fn guard() -> MutexGuard<'static, ()> {
    REBUILD_GUARD.lock().await
}

#[cfg(test)]
#[path = "rebuild-tests.rs"]
mod tests;
