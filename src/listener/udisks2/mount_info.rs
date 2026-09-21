//! Gather information about a partition's `fstab` mount configuration and
//! hand mount, unmount, mount point change and mount options change events
//! off to the user's external library.
//!
//! A device is identified by its filesystem UUID, reported as
//! `/dev/disk/by-uuid/<uuid>`, never by its raw device file. A device is
//! recognised as an unlocked LUKS volume by its `CryptoBackingDevice`
//! property being different from `/`; in that case the UUID used is the
//! *backing* (locked) device's, not the cleartext mapper's, and the mapper
//! and backing device files are reported alongside the mount ([`gather`],
//! [`report_mount`]). Byte-array (`ay`) values returned by UDisks2 over
//! D-Bus are NUL-terminated; `bytes_to_string` strips that trailing byte
//! before lossy UTF-8 decoding. A device with no `fstab` entry is treated as
//! unmounted (`fstab_entry` returns `None`); only the `"fstab"`-kind entry of
//! `Block.Configuration` is considered, its first occurrence if more than one
//! is present, and every other kind (e.g. `"crypttab"`) is filtered out.

use std::collections::HashMap;

use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::proxies::BlockProxy;
use crate::error::Error;

/// Mount information for a partition, ready to hand off to the external library.
///
/// Built by [`gather`] from a device's `Block` properties and its `fstab`
/// entry (`dir`/`opts`). For an unlocked LUKS device this describes the
/// cleartext (mapped) filesystem, but `disk_path` addresses the underlying
/// LUKS container, not the mapper device.
///
/// # Fields
/// See per-field docs below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    /// Mount point configured in `fstab` (`Block.Configuration`'s `dir`),
    /// e.g. `/mnt/data`.
    pub mount_point: String,
    /// Path to the disk, as `/dev/disk/by-uuid/<uuid>`. For an unlocked LUKS
    /// device, `<uuid>` is the locked LUKS container's `IdUUID`, not the
    /// cleartext mapper device's.
    pub disk_path: String,
    /// Filesystem type reported by UDisks2 (`Block.IdType`), e.g. `ext4`,
    /// `vfat`. For the cleartext device of an unlocked LUKS volume, this is
    /// the filesystem inside the container, not `crypto_LUKS`.
    pub filesystem_type: String,
    /// Mount options configured in `fstab` (`Block.Configuration`'s `opts`),
    /// as a single comma-separated string.
    pub options: String,
}

/// Mount information for an unlocked LUKS partition, plus the device names
/// needed to address the mapper (cleartext) and the real (locked) device.
///
/// Built by [`report_mount`] when the `CryptoBackingDevice` gathered
/// alongside a [`MountInfo`] is not `/`.
///
/// # Fields
/// See per-field docs below.
#[derive(Debug, PartialEq, Eq)]
pub struct LuksMountInfo {
    /// Mount information for the filesystem, exactly as for a non-encrypted
    /// device.
    pub mount: MountInfo,
    /// Cleartext (mapper) device file the filesystem is actually mounted
    /// from, e.g. `/dev/mapper/luks-<uuid>` (UDisks2 `PreferredDevice`).
    pub mapper_device: String,
    /// Locked LUKS device file backing `mapper_device`, e.g. `/dev/sda2`
    /// (UDisks2 `Device`).
    pub backing_device: String,
}

/// The `fstab` entry of a `Block.Configuration` value: the configured mount
/// point and mount options.
///
/// Extracted by `fstab_entry` from the `dir`/`opts` details of the
/// `"fstab"`-kind entry, if present.
///
/// # Fields
/// See per-field docs below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FstabEntry {
    /// Configured mount point (`dir`), decoded from its NUL-terminated byte
    /// array.
    pub mount_point: String,
    /// Configured mount options (`opts`), decoded from its NUL-terminated
    /// byte array.
    pub options: String,
}

/// Extract the `fstab` entry from a `Block.Configuration` value, if any.
///
/// # Parameters
/// * `configuration` - the raw value of `Block.Configuration`: a list of
///   `(kind, details)` pairs, one per configuration source (`"fstab"`,
///   `"crypttab"`, ...).
///
/// # Returns
/// `Some(FstabEntry)` built from the first entry whose kind is exactly
/// `"fstab"` and whose `details` map has both a `dir` and an `opts` key
/// convertible to `Vec<u8>`. `None` when no `"fstab"` entry is present, when
/// it is present but is missing `dir` or `opts`, or when either value is not
/// a byte array.
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
///
/// # Parameters
/// * `connection` - D-Bus connection used to build the `Block` proxies for
///   `path` and, for an encrypted device, for its backing device.
/// * `path` - object path of the device whose `fstab` entry is being
///   reported; for an unlocked LUKS volume, the cleartext (mapper) device.
/// * `mount_point` - mount point to embed in the returned [`MountInfo`],
///   normally the `dir` of the device's `fstab` entry.
/// * `options` - mount options to embed in the returned [`MountInfo`],
///   normally the `opts` of the device's `fstab` entry.
///
/// # Returns
/// The gathered [`MountInfo`] together with the device's
/// `CryptoBackingDevice` object path (`/` when `path` is not an encrypted
/// device's cleartext mapper).
///
/// # Errors
/// Any [`Error`] from building a `Block` proxy for `path` or the backing
/// device, or from fetching `IdType`, `CryptoBackingDevice` or `IdUUID` over
/// D-Bus.
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
///
/// # Parameters
/// * `connection` - D-Bus connection used to build the `Block` proxies for
///   `path` and, for a LUKS mount, for `backing_device`.
/// * `path` - object path of the mounted device; for a LUKS mount, the
///   cleartext (mapper) device, whose `PreferredDevice` becomes
///   `LuksMountInfo::mapper_device`.
/// * `info` - mount information gathered by [`gather`] for `path`.
/// * `backing_device` - the `CryptoBackingDevice` gathered alongside `info`;
///   `/` reports a normal mount, anything else reports a LUKS mount and is
///   queried for its `Device` to become `LuksMountInfo::backing_device`.
///
/// # Returns
/// `Ok(())` once the mount has been reported (logged, and printed in release
/// builds).
///
/// # Errors
/// Any [`Error`] from building a `Block` proxy for `path` or
/// `backing_device`, or from fetching `PreferredDevice`/`Device` over D-Bus.
/// Never fails for a non-encrypted mount, which performs no extra D-Bus call.
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
///
/// # Parameters
/// * `info` - mount information previously reported for the partition that
///   is now unmounted.
///
/// # Post-conditions
/// Logs the unmount at info level; in release builds also prints the
/// stubbed library call (`#[cfg(not(debug_assertions))]`).
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
///
/// # Parameters
/// * `info` - previously reported mount information; `info.options` is its
///   old value, logged for context.
/// * `new_options` - the newly configured mount options, replacing
///   `info.options`.
///
/// # Post-conditions
/// Logs the options change at info level, old and new options included; in
/// release builds also prints the stubbed library call
/// (`#[cfg(not(debug_assertions))]`). Does not mutate `info`; the caller is
/// responsible for updating its stored `options`.
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

/// Log and, in release builds, print the stubbed library call for a
/// non-encrypted mount of `info`.
///
/// # Parameters
/// * `info` - mount information to report.
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

/// Log and, in release builds, print the stubbed library call for a LUKS
/// mount described by `info`.
///
/// # Parameters
/// * `info` - mount and device information to report.
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
///
/// # Parameters
/// * `bytes` - raw `ay` (byte array) value as returned by UDisks2, e.g. a
///   `dir`/`opts` `fstab` detail or a `Device`/`PreferredDevice` property.
///
/// # Returns
/// `bytes` decoded as UTF-8, with a single trailing `\0` removed if present.
/// Invalid UTF-8 is replaced lossily (`String::from_utf8_lossy`); this never
/// fails.
fn bytes_to_string(bytes: &[u8]) -> String {
    let trimmed = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    String::from_utf8_lossy(trimmed).into_owned()
}

#[cfg(test)]
#[path = "mount_info-tests.rs"]
mod tests;
