//! Suppression window shared by the two UDisks2 paths, so one user action is
//! reported once.
//!
//! [`super::monitor`] intercepts the `AddConfigurationItem`/
//! `RemoveConfigurationItem`/`UpdateConfigurationItem` method calls, while
//! [`super::watch_configuration`] watches the `Block.Configuration` property
//! those same calls change when UDisks2 manages to write `/etc/fstab`. On a
//! system where that write succeeds both paths see the same user action; on a
//! NixOS system where `/etc/fstab` is a read-only store symlink only the
//! monitor does.
//!
//! The monitor therefore stamps a device here just before it reports, and the
//! property watcher asks [`is_echo`] whether the change it just observed is
//! the echo of a call already reported. The watcher still updates its baseline
//! in that case — only the report to the external library is skipped.
//!
//! The same stamp covers the feedback loop of the write side: reporting a
//! mount eventually rewrites `fstab.nix` and runs `nixos-rebuild`, which
//! changes `/etc/fstab` and can make UDisks2 re-read the very configuration we
//! just applied. [`ECHO_WINDOW`] is sized to outlast a rebuild for that
//! reason.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::OwnedObjectPath;

/// How long a device stays stamped after a method call was reported for it.
///
/// Must outlast the `nixos-rebuild` the report triggers (minutes), since that
/// rebuild is what makes `/etc/fstab` change and the property watcher fire.
const ECHO_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Devices whose configuration was last changed through an intercepted method
/// call, and when.
///
/// Keyed by the device's UDisks2 object path. Entries are pruned lazily by
/// [`is_echo`]; the map therefore holds at most one entry per device touched in
/// the last [`ECHO_WINDOW`].
static RECENT: LazyLock<Mutex<HashMap<OwnedObjectPath, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Stamp `path` as having just had its `fstab` configuration changed through an
/// intercepted method call.
///
/// # Parameters
/// * `path` - UDisks2 object path of the device the method call targeted.
///
/// # Post-conditions
/// Any `Block.Configuration` change observed for `path` within
/// [`ECHO_WINDOW`] is an echo as far as [`is_echo`] is concerned. A previous
/// stamp for the same `path` is overwritten, extending the window.
///
/// # Panics
/// Panics if the internal mutex is poisoned (a previous holder panicked while
/// holding the lock).
pub(super) fn record(path: &OwnedObjectPath) {
    RECENT
        .lock()
        .expect("lock poisoned")
        .insert(path.clone(), Instant::now());
}

/// Whether a `Block.Configuration` change just observed for `path` is the echo
/// of a method call already reported by [`super::monitor`].
///
/// # Parameters
/// * `path` - UDisks2 object path of the device whose `Configuration` changed.
///
/// # Returns
/// `true` when [`record`] stamped `path` less than [`ECHO_WINDOW`] ago, meaning
/// the change must not be reported again; `false` otherwise.
///
/// # Post-conditions
/// Every stamp older than [`ECHO_WINDOW`] is dropped, including for devices
/// other than `path`.
///
/// # Panics
/// Panics if the internal mutex is poisoned (a previous holder panicked while
/// holding the lock).
pub(super) fn is_echo(path: &OwnedObjectPath) -> bool {
    let mut recent = RECENT.lock().expect("lock poisoned");
    let now = Instant::now();

    recent.retain(|_, stamped| now.duration_since(*stamped) < ECHO_WINDOW);
    recent.contains_key(path)
}

#[cfg(test)]
#[path = "recent-tests.rs"]
mod tests;
