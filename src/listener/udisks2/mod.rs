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

const SERVICE: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const FILESYSTEM_INTERFACE: &str = "org.freedesktop.UDisks2.Filesystem";

/// Listener for `org.freedesktop.UDisks2`.
pub struct Udisks2Listener;

#[async_trait]
impl Listener for Udisks2Listener {
    fn name(&self) -> &'static str {
        "udisks2"
    }

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
struct Watchers(Arc<Mutex<HashMap<OwnedObjectPath, JoinHandle<()>>>>);

impl Watchers {
    /// Spawn a watcher for `path`, aborting any previous watcher for the same path.
    ///
    /// `report_initial_config` controls how an already-configured `fstab`
    /// entry baseline is treated: `false` for devices present at startup
    /// (already configured, not a new event), `true` for devices that just
    /// appeared via `InterfacesAdded` (already configured counts as a
    /// configuration that happened just now).
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
    fn remove(&self, path: &OwnedObjectPath) {
        if let Some(handle) = self.0.lock().expect("lock poisoned").remove(path) {
            handle.abort();
        }
    }
}

/// Watch `Block.Configuration` at `path` and report every `fstab` entry add,
/// removal, mount point change and mount options change.
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
