//! Gather information about a partition's `fstab` mount configuration and
//! hand mount, unmount, mount point change and mount options change events
//! off to the user's external library.

use std::collections::HashMap;

use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::proxies::BlockProxy;
use crate::error::Error;

/// Mount information for a partition, ready to hand off to the external library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub mount_point: String,
    pub disk_path: String,
    pub filesystem_type: String,
    pub options: String,
}

/// Mount information for an unlocked LUKS partition, plus the device names
/// needed to address the mapper (cleartext) and the real (locked) device.
#[derive(Debug, PartialEq, Eq)]
pub struct LuksMountInfo {
    pub mount: MountInfo,
    pub mapper_device: String,
    pub backing_device: String,
}

/// The `fstab` entry of a `Block.Configuration` value: the configured mount
/// point and mount options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FstabEntry {
    pub mount_point: String,
    pub options: String,
}

/// Extract the `fstab` entry from a `Block.Configuration` value, if any.
pub(super) fn fstab_entry(
    configuration: &[(String, HashMap<String, OwnedValue>)],
) -> Option<FstabEntry> {
    let (_, details) = configuration.iter().find(|(kind, _)| kind == "fstab")?;
    let dir: Vec<u8> = details.get("dir")?.clone().try_into().ok()?;
    let opts: Vec<u8> = details.get("opts")?.clone().try_into().ok()?;

    Some(FstabEntry {
        mount_point: bytes_to_string(&dir),
        options: bytes_to_string(&opts),
    })
}

/// Gather [`MountInfo`] for the device at `path`, configured with
/// `mount_point` and `options` from its `fstab` entry.
///
/// For an unlocked LUKS device, `path` is the cleartext device; the disk
/// path is derived from its `CryptoBackingDevice` (the locked partition)
/// instead of the cleartext device's own UUID.
///
/// Also returns the device's `CryptoBackingDevice` (`/` when not encrypted),
/// so callers can report a mount without re-fetching it.
pub async fn gather(
    connection: &Connection,
    path: &OwnedObjectPath,
    mount_point: String,
    options: String,
) -> Result<(MountInfo, OwnedObjectPath), Error> {
    let block = BlockProxy::builder(connection).path(path)?.build().await?;

    let filesystem_type = block.id_type().await?;
    let backing_device = block.crypto_backing_device().await?;

    let disk_uuid = if backing_device.as_str() == "/" {
        block.id_uuid().await?
    } else {
        BlockProxy::builder(connection)
            .path(&backing_device)?
            .build()
            .await?
            .id_uuid()
            .await?
    };

    Ok((
        MountInfo {
            mount_point,
            disk_path: format!("/dev/disk/by-uuid/{disk_uuid}"),
            filesystem_type,
            options,
        },
        backing_device,
    ))
}

/// Report a mount of `info` to the external library.
///
/// `backing_device` is the `CryptoBackingDevice` returned alongside `info` by
/// [`gather`]. When it is `/` (not encrypted), reports a normal mount;
/// otherwise reports a LUKS mount, passing the mapper (cleartext) device name
/// and the real (locked) device name separately.
pub async fn report_mount(
    connection: &Connection,
    path: &OwnedObjectPath,
    info: &MountInfo,
    backing_device: &OwnedObjectPath,
) -> Result<(), Error> {
    if backing_device.as_str() == "/" {
        report_normal_mount(info);
        return Ok(());
    }

    let block = BlockProxy::builder(connection).path(path)?.build().await?;
    let mapper_device = bytes_to_string(&block.preferred_device().await?);

    let backing = BlockProxy::builder(connection)
        .path(backing_device)?
        .build()
        .await?;
    let real_device = bytes_to_string(&backing.device().await?);

    report_luks_mount(&LuksMountInfo {
        mount: info.clone(),
        mapper_device,
        backing_device: real_device,
    });

    Ok(())
}

/// Report that the `fstab` mount configuration for `info` was removed,
/// i.e. the partition should be unmounted, to the external library.
pub fn report_unmount(info: &MountInfo) {
    tracing::info!(
        mount_point = %info.mount_point,
        disk_path = %info.disk_path,
        "detected mount configuration removed"
    );

    #[cfg(not(debug_assertions))]
    println!("unmount {} from {}", info.disk_path, info.mount_point);
}

/// Report that the mount options configured for a still-configured `info`
/// changed to `new_options`.
pub fn report_options_changed(info: &MountInfo, new_options: &str) {
    tracing::info!(
        mount_point = %info.mount_point,
        disk_path = %info.disk_path,
        old_options = %info.options,
        new_options = %new_options,
        "detected mount options change"
    );

    #[cfg(not(debug_assertions))]
    println!(
        "remount {} at {} with options={}",
        info.disk_path, info.mount_point, new_options
    );
}

fn report_normal_mount(info: &MountInfo) {
    tracing::info!(
        mount_point = %info.mount_point,
        disk_path = %info.disk_path,
        filesystem_type = %info.filesystem_type,
        options = %info.options,
        "detected mount configuration"
    );

    #[cfg(not(debug_assertions))]
    println!(
        "mount {} at {} (fstype={}, options={})",
        info.disk_path, info.mount_point, info.filesystem_type, info.options
    );
}

fn report_luks_mount(info: &LuksMountInfo) {
    tracing::info!(
        mount_point = %info.mount.mount_point,
        disk_path = %info.mount.disk_path,
        filesystem_type = %info.mount.filesystem_type,
        options = %info.mount.options,
        mapper_device = %info.mapper_device,
        backing_device = %info.backing_device,
        "detected LUKS mount configuration"
    );

    #[cfg(not(debug_assertions))]
    println!(
        "mount-luks {} ({} -> {}) at {} (fstype={}, options={})",
        info.mount.disk_path,
        info.backing_device,
        info.mapper_device,
        info.mount.mount_point,
        info.mount.filesystem_type,
        info.mount.options
    );
}

/// Strip the trailing NUL byte D-Bus uses to terminate `ay`-encoded paths.
fn bytes_to_string(bytes: &[u8]) -> String {
    let trimmed = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    String::from_utf8_lossy(trimmed).into_owned()
}

#[cfg(test)]
#[path = "mount_info-tests.rs"]
mod tests;
