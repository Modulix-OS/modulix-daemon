//! Listeners for `org.freedesktop.UDisks2`.
//!
//! Two complementary paths detect changes to a partition's `fstab` mount
//! configuration and report them to the external library:
//!
//! * [`monitor`] ([`Udisks2MonitorListener`]) intercepts the
//!   `AddConfigurationItem`/`RemoveConfigurationItem`/`UpdateConfigurationItem`
//!   method calls on `org.freedesktop.UDisks2.Block`. **This is the primary
//!   path**: it sees the request itself, so it is unaffected by whether UDisks2
//!   then manages to persist it, it is not subject to the property stream's
//!   coalescing, and — decisively — it cannot be triggered by our own
//!   `nixos-rebuild` (see the feedback loop below).
//! * This module ([`Udisks2Listener`]) watches the resulting
//!   `org.freedesktop.UDisks2.Block.Configuration` property, which covers what
//!   the monitor cannot see: a `/etc/fstab` change made outside D-Bus, and the
//!   state of a device at the moment it appears.
//!
//! Both paths observe the same user action whenever UDisks2 does write
//! `/etc/fstab`, which as root it does: it writes a temporary file and renames
//! it over the path, replacing the NixOS store symlink with a regular file.
//! [`recent`] is what keeps such an action reported once: the monitor stamps
//! the device before reporting, and this module skips reporting a change it
//! recognises as that stamp's echo (its baseline is still updated).
//!
//! # Feedback loop on `/etc/fstab`
//! Reporting a change eventually rewrites `fstab.nix` and runs
//! `nixos-rebuild switch`, whose activation relinks every `/etc` file to the
//! store unconditionally — including the `/etc/fstab` UDisks2 had just
//! replaced. `Block.Configuration` therefore changes again as a consequence of
//! our own report. [`recent`] holds its stamp long enough to outlast a rebuild
//! for that reason, and the monitor path is immune by construction: a rebuild
//! makes no D-Bus method call. Note that until that rebuild completes,
//! `/etc/fstab` is a regular file diverging from the Nix configuration.
//!
//! For the property path: a new `fstab` entry is a mount, a removed entry is an
//! unmount, a changed `dir` is a mount point change (unmount followed by a
//! mount), and a changed `opts` on an otherwise unchanged entry is a mount
//! options change. LUKS partitions are covered the same way: once unlocked, the
//! cleartext mapper device gains its own `Filesystem` interface and its own
//! `Configuration`/`fstab` entry, and is watched identically; [`mount_info`]
//! resolves the disk UUID, mapper device name and real (locked) device name
//! back from the backing device in that case.
//!
//! Devices present at startup whose `fstab` entry is already configured are
//! not reported (no configuration change happened during our lifetime).
//! Devices that *appear* via `InterfacesAdded` already configured — e.g. a
//! LUKS device whose mapper comes up with its `fstab` entry already in place
//! — are reported as a mount, since that configuration did appear while we
//! were watching.
//!
//! # Subscriptions
//! [`Udisks2Listener::listen`] subscribes to the `org.freedesktop.UDisks2`
//! [`ObjectManagerProxy`]'s `InterfacesAdded`/`InterfacesRemoved` signals at
//! `/org/freedesktop/UDisks2`, to start/stop watching a device as its
//! `Filesystem` interface appears/disappears. Each watched device additionally
//! gets its own `tokio` task (spawned by `Watchers::spawn`, running
//! `watch_configuration`) subscribed to the `PropertiesChanged` signal for its
//! `Block.Configuration` property.
//!
//! # Reporting and blocking
//! Every reported mount/unmount/options-change event is handed off to the
//! external library (currently stubbed as a log line plus, unless
//! [`crate::dry_run::is_dry_run`] is true, a `println!`; see `mount_info`).
//! Once wired to the real library, that call edits the NixOS configuration and
//! runs `nixos-rebuild`, which blocks for minutes; that call happens
//! synchronously on the per-device watcher task, so only that device's task is
//! blocked for the duration — other devices' watcher tasks run independently
//! and keep processing their own events.
//!
//! # Coalescing
//! `Configuration` changes are read one at a time, and the report for one is
//! awaited in full before the next change is read. `zbus` does **not** queue
//! property updates though: a `PropertyStream` only keeps the latest value, so
//! changes arriving while the previous one is still being processed are merged
//! rather than replayed one by one. This is harmless here because the logic is
//! edge-based — each iteration compares the recorded baseline against a freshly
//! read value — so the reports always converge on the current configuration,
//! but no intermediate state is guaranteed to be seen.
//!
//! # UDisks2 not running
//! [`Udisks2Listener::listen`] fetches the initial device list with
//! `get_managed_objects` right after building the [`ObjectManagerProxy`]. If
//! `org.freedesktop.UDisks2` cannot be reached at that point, this call fails
//! and its error is propagated out of `listen` immediately: the listener
//! never starts watching any device and the
//! `InterfacesAdded`/`InterfacesRemoved` subscription is never set up.
//! [`Udisks2MonitorListener`] has no such requirement.

mod apply;
mod intent;
mod monitor;
mod mount_info;
mod pending;
mod proxies;
mod recent;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use zbus::Connection;
use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::OwnedObjectPath;

use super::Listener;
use crate::error::Error;
use mount_info::{FstabEntry, FstabLookup, MountInfo};
use proxies::BlockProxy;

pub use monitor::Udisks2MonitorListener;

/// D-Bus service name this listener watches.
const SERVICE: &str = "org.freedesktop.UDisks2";
/// Object path of the `org.freedesktop.DBus.ObjectManager` queried for the
/// initial device list and subscribed to for
/// `InterfacesAdded`/`InterfacesRemoved`.
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
/// Interface a device must expose for its `Block.Configuration` to be
/// watched; its presence/absence in `InterfacesAdded`/`InterfacesRemoved` is
/// what starts/stops that device's `watch_configuration` task.
const FILESYSTEM_INTERFACE: &str = "org.freedesktop.UDisks2.Filesystem";

/// Listener for `org.freedesktop.UDisks2.Block.Configuration`.
///
/// Zero-sized: all state lives in the `Connection` passed to
/// [`Listener::listen`] and in the `Watchers` created for the duration of
/// that call.
pub struct Udisks2Listener;

#[async_trait]
impl Listener for Udisks2Listener {
    /// Returns `"udisks2"`, this listener's identifier used in logs.
    fn name(&self) -> &'static str {
        "udisks2"
    }

    /// Subscribes to `org.freedesktop.UDisks2`'s `ObjectManager`, starts
    /// watching every already-present device exposing a `Filesystem`
    /// interface, then keeps watching devices as they gain/lose that
    /// interface for as long as `connection` stays open.
    ///
    /// # Parameters
    /// * `connection` - system bus connection to subscribe on.
    ///
    /// # Pre-conditions
    /// `org.freedesktop.UDisks2` must be reachable on `connection`'s bus: the
    /// initial `get_managed_objects` call is not retried.
    ///
    /// # Post-conditions
    /// Returns `Ok(())` once both signal streams have ended (`connection`
    /// closed); until then, loops forever over `InterfacesAdded`/
    /// `InterfacesRemoved`, starting or stopping a `watch_configuration` task
    /// (via `Watchers`) as each device's `Filesystem` interface
    /// appears/disappears.
    ///
    /// # Errors
    /// Returns immediately with an error if the `ObjectManagerProxy` cannot
    /// be built, if the initial `get_managed_objects` call fails (e.g.
    /// `org.freedesktop.UDisks2` is not running/reachable), or if
    /// subscribing to `InterfacesAdded`/`InterfacesRemoved` fails. Once the
    /// loop is running, a failure to decode a received signal's arguments
    /// also ends `listen` with that error.
    async fn listen(&self, connection: Connection) -> Result<(), Error> {
        let object_manager = ObjectManagerProxy::builder(&connection)
            .destination(SERVICE)?
            .path(MANAGER_PATH)?
            .build()
            .await?;

        let watchers = Watchers::default();

        let managed_objects = object_manager
            .get_managed_objects()
            .await
            .map_err(zbus::Error::from)?;

        let mut watched = 0u32;
        for (path, interfaces) in managed_objects {
            if interfaces
                .keys()
                .any(|i| i.as_str() == FILESYSTEM_INTERFACE)
            {
                watchers.spawn(&connection, path, false);
                watched += 1;
            }
        }
        tracing::info!(watched, "udisks2 listener started");

        let mut added = object_manager.receive_interfaces_added().await?;
        let mut removed = object_manager.receive_interfaces_removed().await?;

        loop {
            tokio::select! {
                Some(signal) = added.next() => {
                    let args = signal.args()?;
                    if args.interfaces_and_properties().keys().any(|i| i.as_str() == FILESYSTEM_INTERFACE) {
                        watchers.spawn(&connection, OwnedObjectPath::from(args.object_path().to_owned()), true);
                    }
                }
                Some(signal) = removed.next() => {
                    let args = signal.args()?;
                    if args.interfaces().iter().any(|i| i.as_str() == FILESYSTEM_INTERFACE) {
                        watchers.remove(&OwnedObjectPath::from(args.object_path().to_owned()));
                    }
                }
                else => return Ok(()),
            }
        }
    }
}

/// Per-object watcher tasks for `Block.Configuration`, keyed by object path
/// so they can be aborted when the device disappears.
#[derive(Default)]
struct Watchers(
    /// Map from a watched device's object path to the `tokio` task running
    /// `watch_configuration` for it. Behind `Arc<Mutex<_>>` so the spawned
    /// tasks and the signal-handling loop can share it.
    Arc<Mutex<HashMap<OwnedObjectPath, JoinHandle<()>>>>,
);

impl Watchers {
    /// Spawn a watcher for `path`, unless one is already running for it.
    ///
    /// A device can be reported both by the initial `get_managed_objects` list
    /// and by an `InterfacesAdded` signal. Keeping the running watcher rather
    /// than replacing it preserves its baseline, which is what stops an
    /// already-configured device from being reported as a fresh mount. A
    /// watcher that has already exited (it failed, see the post-conditions) is
    /// replaced.
    ///
    /// `report_initial_config` controls how an already-configured `fstab`
    /// entry baseline is treated: `false` for devices present at startup
    /// (already configured, not a new event), `true` for devices that just
    /// appeared via `InterfacesAdded` (already configured counts as a
    /// configuration that happened just now).
    ///
    /// # Parameters
    /// * `connection` - system bus connection the spawned task uses to watch
    ///   `path` and, when reporting a mount, to query the device.
    /// * `path` - object path of the device to watch.
    /// * `report_initial_config` - as described above.
    ///
    /// # Post-conditions
    /// A `watch_configuration` task for `path` is running: either the one that
    /// already was, or a newly spawned one. A task that later fails (any `Err`
    /// from `watch_configuration`, e.g. a lost D-Bus connection) logs the error
    /// and exits; it stays in this map until the device disappears or until
    /// this method is called again for the same `path`, which then replaces it.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned (a previous holder panicked
    /// while holding the lock).
    fn spawn(&self, connection: &Connection, path: OwnedObjectPath, report_initial_config: bool) {
        let mut watchers = self.0.lock().expect("lock poisoned");

        if watchers.get(&path).is_some_and(|w| !w.is_finished()) {
            tracing::debug!(path = %path, "udisks2: already watched, keeping its baseline");
            return;
        }

        let connection = connection.clone();
        let task_path = path.clone();
        let handle = tokio::spawn(async move {
            if let Err(err) =
                watch_configuration(&connection, &task_path, report_initial_config).await
            {
                tracing::error!(path = %task_path, %err, "udisks2 configuration watcher failed");
            }
        });

        if let Some(previous) = watchers.insert(path, handle) {
            previous.abort();
        }
    }

    /// Abort and drop the watcher for `path`, if any.
    ///
    /// # Parameters
    /// * `path` - object path whose watcher must stop.
    ///
    /// # Post-conditions
    /// No watcher task remains registered for `path`; its `watch_configuration`
    /// task is aborted immediately (no graceful shutdown, no final report for
    /// whatever change it may have been mid-processing). A `path` with no
    /// registered watcher is a no-op.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned (a previous holder panicked
    /// while holding the lock).
    fn remove(&self, path: &OwnedObjectPath) {
        if let Some(handle) = self.0.lock().expect("lock poisoned").remove(path) {
            handle.abort();
        }
    }
}

/// Watch `Block.Configuration` at `path` and report every `fstab` entry add,
/// removal, mount point change and mount options change.
///
/// # Parameters
/// * `connection` - system bus connection used to build the `BlockProxy` for
///   `path` and to subscribe to its `Configuration` property changes.
/// * `path` - object path of the device to watch; the cleartext mapper
///   device path for an unlocked LUKS partition, the plain block device path
///   otherwise.
/// * `report_initial_config` - if `true`, an already-configured `fstab` entry
///   found on the very first `Configuration` value read is reported as a
///   mount (device just appeared via `InterfacesAdded`); if `false`, it is
///   only recorded as the current baseline and not reported (device was
///   already present when the listener started).
///
/// # Returns
/// `Ok(())` once the `Configuration` change stream ends (e.g. the D-Bus
/// connection closes) without any of the errors below occurring.
///
/// # Post-conditions
/// Runs for as long as the `Configuration` property-change stream keeps
/// yielding, processing one change at a time: gathering [`MountInfo`] and
/// reporting to the external library are awaited in full before the next
/// change is read off the stream (see the module-level docs on coalescing).
/// A change [`recent::is_echo`] recognises as the echo of an already-reported
/// method call still updates the baseline but is not reported again, and
/// neither is a `Configuration` whose `fstab` entry is present but unreadable
/// ([`FstabLookup::Malformed`]) — that leaves the baseline untouched rather
/// than passing for an unmount. A failure to *report* a mount
/// (`mount_info::report_mount` returning `Err`) is only logged: it does not
/// stop the loop, and the baseline is still updated as if the report had
/// succeeded.
///
/// # Errors
/// Propagates any error from building the `BlockProxy`, from reading a
/// `Configuration` change's value, or from `mount_info::gather`. Any such
/// error ends the watch for `path` (the caller, `Watchers::spawn`, logs it
/// and does not restart the task).
async fn watch_configuration(
    connection: &Connection,
    path: &OwnedObjectPath,
    report_initial_config: bool,
) -> Result<(), Error> {
    tracing::info!(path = %path, "udisks2: watching block configuration");

    let block = BlockProxy::builder(connection).path(path)?.build().await?;
    let mut configuration_changes = block.receive_configuration_changed().await;

    let mut current: Option<MountInfo> = None;
    let mut first = true;

    while let Some(change) = configuration_changes.next().await {
        let configuration = change.get().await?;
        let lookup = mount_info::fstab_entry(&configuration);
        tracing::info!(
            path = %path,
            lookup = ?lookup,
            first,
            "udisks2: Configuration changed"
        );

        if first {
            first = false;
            if let FstabLookup::Entry(entry) = lookup {
                current = Some(mount(connection, &block, entry, !report_initial_config).await?);
            }
            continue;
        }

        let suppress = recent::is_echo(path);
        if suppress {
            tracing::debug!(path = %path, "udisks2: echo of an intercepted call, not reported");
        }

        match (current.take(), lookup) {
            (previous, FstabLookup::Malformed) => {
                tracing::warn!(path = %path, "udisks2: unreadable fstab entry, state left unchanged");
                current = previous;
            }
            (None, FstabLookup::Absent) => {}
            (None, FstabLookup::Entry(entry)) => {
                current = Some(mount(connection, &block, entry, suppress).await?);
            }
            (Some(info), FstabLookup::Absent) => {
                if !suppress && let Err(err) = mount_info::report_unmount(&info).await {
                    tracing::error!(%err, "failed to report partition unmount");
                }
            }
            (Some(info), FstabLookup::Entry(entry)) if info.mount_point != entry.mount_point => {
                if !suppress && let Err(err) = mount_info::report_unmount(&info).await {
                    tracing::error!(%err, "failed to report partition unmount");
                }
                current = Some(mount(connection, &block, entry, suppress).await?);
            }
            (Some(info), FstabLookup::Entry(entry)) => {
                current = Some(remount(connection, &block, info, entry, suppress).await?);
            }
        }
    }

    Ok(())
}

/// Gather the [`MountInfo`] for `entry` and, unless `suppress`, report it as a
/// mount.
///
/// # Parameters
/// * `connection` - connection used to query the backing device of an
///   encrypted device.
/// * `block` - `Block` proxy for the device being mounted.
/// * `entry` - the device's `fstab` entry, source of the mount point and
///   options.
/// * `suppress` - when `true`, gather but do not report (the change is a
///   baseline, or the echo of an already-reported method call).
///
/// # Returns
/// The gathered [`MountInfo`], to become the caller's new baseline.
///
/// # Post-conditions
/// A failure to report is logged, not propagated: the returned [`MountInfo`]
/// is the new baseline either way.
///
/// # Errors
/// Any [`Error`] from `mount_info::gather`.
async fn mount(
    connection: &Connection,
    block: &BlockProxy<'_>,
    entry: FstabEntry,
    suppress: bool,
) -> Result<MountInfo, Error> {
    let (info, backing_device) =
        mount_info::gather(connection, block, entry.mount_point, entry.options).await?;

    if !suppress
        && let Err(err) = mount_info::report_mount(connection, block, &info, &backing_device).await
    {
        tracing::error!(%err, "failed to report partition mount");
    }

    Ok(info)
}

/// Re-gather a still-configured device whose mount point did not change, and
/// report whatever actually changed.
///
/// Re-gathering rather than only diffing `entry` against `info` is what catches
/// a device swap at the same mount point: `Block.IdUUID` and `Block.IdType` are
/// not part of the `fstab` entry, so a changed disk or filesystem type is
/// otherwise invisible.
///
/// # Parameters
/// * `connection` - connection used to query the backing device of an
///   encrypted device.
/// * `block` - `Block` proxy for the device.
/// * `info` - the current baseline for this device.
/// * `entry` - its freshly read `fstab` entry; same mount point as `info`.
/// * `suppress` - when `true`, re-gather but do not report.
///
/// # Returns
/// The freshly gathered [`MountInfo`], to become the caller's new baseline.
///
/// # Post-conditions
/// A different disk or filesystem type is reported as an unmount of `info`
/// followed by a mount of the fresh information; otherwise a different set of
/// options (see `mount_info::same_options`) is reported as an options change,
/// and an unchanged device is not reported at all. A failure to report is
/// logged, not propagated.
///
/// # Errors
/// Any [`Error`] from `mount_info::gather`.
async fn remount(
    connection: &Connection,
    block: &BlockProxy<'_>,
    info: MountInfo,
    entry: FstabEntry,
    suppress: bool,
) -> Result<MountInfo, Error> {
    let (fresh, backing_device) =
        mount_info::gather(connection, block, entry.mount_point, entry.options).await?;

    if suppress {
        return Ok(fresh);
    }

    if fresh.disk_path != info.disk_path || fresh.filesystem_type != info.filesystem_type {
        if let Err(err) = mount_info::report_unmount(&info).await {
            tracing::error!(%err, "failed to report partition unmount");
        }
        if let Err(err) = mount_info::report_mount(connection, block, &fresh, &backing_device).await
        {
            tracing::error!(%err, "failed to report partition mount");
        }
    } else if !mount_info::same_options(&info.options, &fresh.options)
        && let Err(err) = mount_info::report_options_changed(&info, &fresh.options).await
    {
        tracing::error!(%err, "failed to report partition options change");
    }

    Ok(fresh)
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
