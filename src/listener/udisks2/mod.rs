//! Listener for `org.freedesktop.UDisks2`.
//!
//! Detects changes to a partition's `fstab` mount configuration
//! (`org.freedesktop.UDisks2.Block.Configuration`) and reports them to the
//! external library: a new `fstab` entry is a mount, a removed entry is an
//! unmount, a changed `dir` is a mount point change (unmount followed by a
//! mount), and a changed `opts` on an otherwise unchanged entry is a mount
//! options change. LUKS partitions are covered the same way: once unlocked,
//! the cleartext mapper device gains its own `Filesystem` interface and its
//! own `Configuration`/`fstab` entry, and is watched identically;
//! [`mount_info`] resolves the disk UUID, mapper device name and real
//! (locked) device name back from the backing device in that case.
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
//! external library (currently stubbed as a log line plus, in release
//! builds, a `println!`; see `mount_info`). Once wired to the real library,
//! that call edits the NixOS configuration and runs `nixos-rebuild`, which
//! blocks for minutes; that call happens synchronously on the per-device
//! watcher task, so only that device's task is blocked for the duration —
//! other devices' watcher tasks run independently and keep processing their
//! own events.
//!
//! # No debouncing or coalescing
//! `Configuration` changes are read one at a time, in the order the D-Bus
//! signal stream delivers them (`while let Some(change) =
//! configuration_changes.next().await` in `watch_configuration`). A change
//! that arrives while the previous one is still being processed is not
//! dropped or merged with it: it queues up in the signal stream's internal
//! buffer and is picked up, and reported, on the next loop iteration, once
//! the current one has finished — including its (blocking, once wired)
//! library call.
//!
//! # UDisks2 not running
//! [`Udisks2Listener::listen`] fetches the initial device list with
//! `get_managed_objects` right after building the [`ObjectManagerProxy`]. If
//! `org.freedesktop.UDisks2` cannot be reached at that point, this call fails
//! and its error is propagated out of `listen` immediately: the listener
//! never starts watching any device and the
//! `InterfacesAdded`/`InterfacesRemoved` subscription is never set up.

mod mount_info;
mod proxies;

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
use mount_info::MountInfo;
use proxies::BlockProxy;

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

/// Listener for `org.freedesktop.UDisks2`.
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
    /// Only returns if `connection` closes or a signal subscription/decoding
    /// fails; otherwise loops forever over `InterfacesAdded`/
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
            }
        }
    }
}

/// Per-object watcher tasks for `Block.Configuration`, keyed by object path
/// so they can be aborted when the device disappears.
#[derive(Default, Clone)]
struct Watchers(
    /// Map from a watched device's object path to the `tokio` task running
    /// `watch_configuration` for it. Shared (`Arc<Mutex<_>>`) so the same
    /// `Watchers` handle can be cloned into the signal-handling loop while
    /// still being read/written from both `spawn` and `remove`.
    Arc<Mutex<HashMap<OwnedObjectPath, JoinHandle<()>>>>,
);

impl Watchers {
    /// Spawn a watcher for `path`, aborting any previous watcher for the same path.
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
    /// A `watch_configuration` task for `path` is running, replacing any
    /// previous one registered for the same `path`, which is aborted. A task
    /// that later fails (any `Err` from `watch_configuration`, e.g. a lost
    /// D-Bus connection) logs the error and exits; it is not restarted or
    /// removed from this map by itself.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned (a previous holder panicked
    /// while holding the lock).
    fn spawn(&self, connection: &Connection, path: OwnedObjectPath, report_initial_config: bool) {
        let connection = connection.clone();
        let task_path = path.clone();
        let handle = tokio::spawn(async move {
            if let Err(err) =
                watch_configuration(&connection, &task_path, report_initial_config).await
            {
                tracing::error!(path = %task_path, %err, "udisks2 configuration watcher failed");
            }
        });

        if let Some(previous) = self.0.lock().expect("lock poisoned").insert(path, handle) {
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
/// yielding, processing one change at a time: gathering `MountInfo` and
/// reporting a mount/unmount/options-change to the external library are
/// awaited in full before the next change is read off the stream, so changes
/// are neither debounced nor coalesced — one arriving mid-processing is
/// simply queued in the stream and handled on the next iteration. A failure
/// to *report* a mount (`mount_info::report_mount` returning `Err`) is only
/// logged: it does not stop the loop, and `current` is still updated to the
/// new entry as if the report had succeeded.
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
    tracing::debug!(path = %path, "udisks2: watching block configuration");

    let block = BlockProxy::builder(connection).path(path)?.build().await?;
    let mut configuration_changes = block.receive_configuration_changed().await;

    let mut current: Option<MountInfo> = None;
    let mut first = true;

    while let Some(change) = configuration_changes.next().await {
        let configuration = change.get().await?;
        let entry = mount_info::fstab_entry(&configuration);
        tracing::debug!(path = %path, configured = entry.is_some(), first, "udisks2: Configuration changed");

        if first {
            if let Some(entry) = entry {
                let (info, backing_device) =
                    mount_info::gather(connection, path, entry.mount_point, entry.options).await?;
                if report_initial_config
                    && let Err(err) =
                        mount_info::report_mount(connection, path, &info, &backing_device).await
                {
                    tracing::error!(path = %path, %err, "failed to report partition mount");
                }
                current = Some(info);
            }
            first = false;
            continue;
        }

        match (current.take(), entry) {
            (None, None) => {}
            (None, Some(entry)) => {
                let (info, backing_device) =
                    mount_info::gather(connection, path, entry.mount_point, entry.options).await?;
                if let Err(err) =
                    mount_info::report_mount(connection, path, &info, &backing_device).await
                {
                    tracing::error!(path = %path, %err, "failed to report partition mount");
                }
                current = Some(info);
            }
            (Some(info), None) => {
                mount_info::report_unmount(&info);
            }
            (Some(info), Some(entry)) if info.mount_point != entry.mount_point => {
                mount_info::report_unmount(&info);

                let (info, backing_device) =
                    mount_info::gather(connection, path, entry.mount_point, entry.options).await?;
                if let Err(err) =
                    mount_info::report_mount(connection, path, &info, &backing_device).await
                {
                    tracing::error!(path = %path, %err, "failed to report partition mount");
                }
                current = Some(info);
            }
            (Some(mut info), Some(entry)) => {
                if info.options != entry.options {
                    mount_info::report_options_changed(&info, &entry.options);
                    info.options = entry.options;
                }
                current = Some(info);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
