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
//! unmounted ([`fstab_entry`] returns [`FstabLookup::Absent`]); only the
//! `"fstab"`-kind entry of `Block.Configuration` is considered, its first
//! occurrence if more than one is present, and every other kind (e.g.
//! `"crypttab"`) is filtered out. An entry that is present but unreadable is
//! reported as [`FstabLookup::Malformed`] and must not be confused with an
//! absent one.

use std::collections::{BTreeSet, HashMap};

use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::apply;
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
/// Extracted by [`fstab_details`] from the `dir`/`opts` details of the
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

/// Outcome of looking for the `"fstab"`-kind entry of a `Block.Configuration`
/// value.
///
/// Distinguishing [`FstabLookup::Absent`] from [`FstabLookup::Malformed`]
/// matters: the former means the device is not configured for mounting (so a
/// device that previously was is being unmounted), while the latter means
/// UDisks2 reported an entry this listener cannot read, and the previously
/// known state must then be left untouched rather than interpreted as an
/// unmount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FstabLookup {
    /// No `"fstab"`-kind entry at all: the device has no `fstab` mount
    /// configuration.
    Absent,
    /// An `"fstab"`-kind entry whose `dir` and `opts` were both readable.
    Entry(FstabEntry),
    /// An `"fstab"`-kind entry is present but unusable: `dir` or `opts` is
    /// missing, or is not a byte array.
    Malformed,
}

/// Extract the `fstab` entry from a `Block.Configuration` value.
///
/// # Parameters
/// * `configuration` - the raw value of `Block.Configuration`: a list of
///   `(kind, details)` pairs, one per configuration source (`"fstab"`,
///   `"crypttab"`, ...).
///
/// # Returns
/// [`FstabLookup::Absent`] when no `"fstab"`-kind entry is present;
/// otherwise the result of [`fstab_details`] on the first `"fstab"`-kind
/// entry's `details`, as [`FstabLookup::Entry`] or [`FstabLookup::Malformed`].
pub(super) fn fstab_entry(configuration: &[(String, HashMap<String, OwnedValue>)]) -> FstabLookup {
    let Some((_, details)) = configuration.iter().find(|(kind, _)| kind == "fstab") else {
        return FstabLookup::Absent;
    };

    match fstab_details(details) {
        Some(entry) => FstabLookup::Entry(entry),
        None => FstabLookup::Malformed,
    }
}

/// Build an [`FstabEntry`] from the `details` map of an `"fstab"`-kind
/// configuration item.
///
/// Shared by [`fstab_entry`], which reads the `Block.Configuration` property,
/// and by [`super::monitor`], which reads the same `a{sv}` map straight out of
/// an intercepted `AddConfigurationItem`/`RemoveConfigurationItem`/
/// `UpdateConfigurationItem` method call.
///
/// # Parameters
/// * `details` - the `a{sv}` details of an `"fstab"`-kind configuration item;
///   UDisks2 populates `fsname`, `dir`, `type`, `opts`, `freq` and `passno`,
///   of which only `dir` and `opts` are read here.
///
/// # Returns
/// `Some(FstabEntry)` when both `dir` and `opts` are present and convertible
/// to `Vec<u8>`, their NUL-terminated byte arrays decoded by
/// [`bytes_to_string`]. `None` when either is missing or is not a byte array.
pub(super) fn fstab_details(details: &HashMap<String, OwnedValue>) -> Option<FstabEntry> {
    let dir: Vec<u8> = details.get("dir")?.clone().try_into().ok()?;
    let opts: Vec<u8> = details.get("opts")?.clone().try_into().ok()?;

    Some(FstabEntry {
        mount_point: bytes_to_string(&dir),
        options: bytes_to_string(&opts),
    })
}

/// Whether two comma-separated `fstab` mount option strings describe the same
/// set of options.
///
/// UDisks2 reports `opts` verbatim from `/etc/fstab`, so a pure reordering of
/// otherwise unchanged options would look like an options change and trigger a
/// needless `nixos-rebuild`.
///
/// # Parameters
/// * `a` - one comma-separated option list, e.g. `"noatime,nofail"`.
/// * `b` - the other comma-separated option list.
///
/// # Returns
/// `true` when `a` and `b` contain the same options, in any order and
/// regardless of duplicates; `false` otherwise. Empty segments (as produced by
/// a trailing or doubled comma) are ignored.
pub(super) fn same_options(a: &str, b: &str) -> bool {
    fn set(options: &str) -> BTreeSet<&str> {
        options.split(',').filter(|o| !o.is_empty()).collect()
    }

    set(a) == set(b)
}

/// Gather [`MountInfo`] for the device proxied by `block`, configured with
/// `mount_point` and `options` from its `fstab` entry.
///
/// For an unlocked LUKS device, `block` proxies the cleartext device; the disk
/// path is derived from its `CryptoBackingDevice` (the locked partition)
/// instead of the cleartext device's own UUID.
///
/// Also returns the device's `CryptoBackingDevice` (`/` when not encrypted),
/// so callers can report a mount without re-fetching it.
///
/// # Parameters
/// * `connection` - D-Bus connection used to build the `Block` proxy for the
///   backing device of an encrypted device; unused otherwise.
/// * `block` - `Block` proxy for the device whose `fstab` entry is being
///   reported, built once by the caller; for an unlocked LUKS volume, the
///   cleartext (mapper) device.
/// * `mount_point` - mount point to embed in the returned [`MountInfo`],
///   normally the `dir` of the device's `fstab` entry.
/// * `options` - mount options to embed in the returned [`MountInfo`],
///   normally the `opts` of the device's `fstab` entry.
///
/// # Returns
/// The gathered [`MountInfo`] together with the device's
/// `CryptoBackingDevice` object path (`/` when `block` is not an encrypted
/// device's cleartext mapper).
///
/// # Errors
/// Any [`Error`] from building a `Block` proxy for the backing device, or
/// from fetching `IdType`, `CryptoBackingDevice` or `IdUUID` over D-Bus.
pub(super) async fn gather(
    connection: &Connection,
    block: &BlockProxy<'_>,
    mount_point: String,
    options: String,
) -> Result<(MountInfo, OwnedObjectPath), Error> {
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

/// Report a mount of `info`, writing it into the NixOS configuration.
///
/// `backing_device` is the `CryptoBackingDevice` returned alongside `info` by
/// [`gather`]. When it is `/` (not encrypted), reports a normal mount;
/// otherwise reports a LUKS mount, resolving the mapper (cleartext) device name
/// and the real (locked) device name first.
///
/// # Parameters
/// * `connection` - D-Bus connection used to build the `Block` proxy for
///   `backing_device` on a LUKS mount; unused otherwise.
/// * `block` - `Block` proxy for the mounted device, built once by the
///   caller; for a LUKS mount, the cleartext (mapper) device, whose
///   `PreferredDevice` becomes `LuksMountInfo::mapper_device`.
/// * `info` - mount information gathered by [`gather`] for `block`.
/// * `backing_device` - the `CryptoBackingDevice` gathered alongside `info`;
///   `/` reports a normal mount, anything else reports a LUKS mount and is
///   queried for its `Device` to become `LuksMountInfo::backing_device`.
///
/// # Returns
/// `Ok(())` once the mount has been declared and the resulting
/// `nixos-rebuild switch` has completed (or was skipped, see
/// [`super::apply::mount`]).
///
/// # Errors
/// Any [`Error`] from building a `Block` proxy for `backing_device`, from
/// fetching `PreferredDevice`/`Device` over D-Bus, or from writing the
/// configuration.
pub(super) async fn report_mount(
    connection: &Connection,
    block: &BlockProxy<'_>,
    info: &MountInfo,
    backing_device: &OwnedObjectPath,
) -> Result<(), Error> {
    if backing_device.as_str() == "/" {
        return report_normal_mount(info).await;
    }

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
    })
    .await
}

/// Report that the `fstab` mount configuration for `info` was removed, i.e.
/// the partition should be unmounted.
///
/// # Parameters
/// * `info` - mount information previously reported for the partition that
///   is now unmounted.
///
/// # Returns
/// `Ok(())` once the mount point has been dropped from the configuration.
///
/// # Post-conditions
/// Logs the unmount at info level, then hands it to [`super::apply::unmount`].
/// A LUKS mount's `boot.initrd.luks.devices` entry goes with it when no other
/// mount point still needs it — that is core-utils' call, not this module's.
///
/// # Errors
/// Any [`Error`] from writing the configuration.
pub(super) async fn report_unmount(info: &MountInfo) -> Result<(), Error> {
    tracing::info!(
        mount_point = %info.mount_point,
        disk_path = %info.disk_path,
        "detected mount configuration removed"
    );

    apply::unmount(info).await
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
/// # Returns
/// `Ok(())` once the mount point has been re-declared with `new_options`.
///
/// # Post-conditions
/// Logs the options change at info level, old and new options included, then
/// re-declares the whole entry: `filesystem::add_entry` resets `.options`
/// before writing, so the declared list ends up being exactly `new_options`.
/// Does not mutate `info`; the caller is responsible for updating its stored
/// `options`.
///
/// # Errors
/// Any [`Error`] from writing the configuration.
pub(super) async fn report_options_changed(
    info: &MountInfo,
    new_options: &str,
) -> Result<(), Error> {
    tracing::info!(
        mount_point = %info.mount_point,
        disk_path = %info.disk_path,
        old_options = %info.options,
        new_options = %new_options,
        "detected mount options change"
    );

    let updated = MountInfo {
        options: new_options.to_string(),
        ..info.clone()
    };

    apply::mount(&updated, None).await
}

/// Log and write a non-encrypted mount of `info`.
///
/// # Parameters
/// * `info` - mount information to report.
///
/// # Returns
/// `Ok(())` once the mount point has been declared.
///
/// # Errors
/// Any [`Error`] from writing the configuration.
async fn report_normal_mount(info: &MountInfo) -> Result<(), Error> {
    tracing::info!(
        mount_point = %info.mount_point,
        disk_path = %info.disk_path,
        filesystem_type = %info.filesystem_type,
        options = %info.options,
        "detected mount configuration"
    );

    apply::mount(info, None).await
}

/// Log and write a LUKS mount described by `info`.
///
/// # Parameters
/// * `info` - mount and device information to report.
///
/// # Returns
/// `Ok(())` once the mount point and its
/// `boot.initrd.luks.devices."<mapper>"` entry have been declared.
///
/// # Errors
/// Any [`Error`] from writing the configuration, including
/// [`Error::CoreUtils`] when no mapper name can be derived from
/// `info.mapper_device` and `info.mount.disk_path`.
async fn report_luks_mount(info: &LuksMountInfo) -> Result<(), Error> {
    tracing::info!(
        mount_point = %info.mount.mount_point,
        disk_path = %info.mount.disk_path,
        filesystem_type = %info.mount.filesystem_type,
        options = %info.mount.options,
        mapper_device = %info.mapper_device,
        backing_device = %info.backing_device,
        "detected LUKS mount configuration"
    );

    apply::mount(&info.mount, Some(&info.mapper_device)).await
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
