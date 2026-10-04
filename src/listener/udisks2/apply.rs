//! Write a reported mount change into the Modulix NixOS configuration.
//!
//! This is the write half of the UDisks2 listeners: [`mount_info`]'s `report_*`
//! functions decide *what* happened, this module makes it stick by editing
//! `fstab.nix` through [`modulix_core_utils::filesystem`] and running the
//! resulting `nixos-rebuild switch`.
//!
//! # Blocking and serialisation
//! `filesystem::add_entry`/`remove_entry` are synchronous and each opens its
//! own transaction plus a full `nixos-rebuild switch`, which blocks for
//! minutes. They therefore run on [`tokio::task::spawn_blocking`], the same way
//! the own-interface commands do (see [`crate::lifecycle_commands`]), and one
//! at a time behind [`APPLY_GUARD`]: two concurrent rebuilds on the same
//! configuration repository would fight over the same git tree.
//!
//! # Dry run
//! Every call is skipped — logged only — when [`crate::dry_run::is_dry_run`] is
//! true, exactly like the own-interface commands.
//!
//! # No LUKS logic here
//! This module states facts and nothing else. UDisks2 is what knows whether a
//! device is an unlocked LUKS volume, which container backs it and which mapper
//! it is open as ([`super::mount_info`] reads those properties); everything
//! that *follows* from those facts — the mapper name to declare, the
//! `boot.initrd.luks.devices` entry, the TPM2 attribute, and dropping that
//! entry again on removal — belongs to
//! [`modulix_core_utils::filesystem::MountDevice`]. The daemon never names a
//! mapper nor spells a Nix option path.
//!
//! # Known limitations
//! * **One rebuild per entry.** `modulix-core-utils` only exposes the
//!   transactional wrappers (the `*_no_transaction` variants take a `NixFile`,
//!   which is not public), so several mount points changed in a row cannot
//!   share one rebuild.
//! * **No read side.** `filesystem` exposes no `list_entries`/`get_entry`, so
//!   this module cannot tell whether the configuration it is about to write is
//!   already there. `remove_mount` in particular rebuilds even when it removed
//!   nothing.

use modulix_core_utils::filesystem::{self, MountDevice};
use tokio::sync::Mutex;

use super::mount_info::MountInfo;
use crate::error::Error;

/// Serialises the configuration transactions this module starts.
///
/// `modulix-core-utils` has its own inter-process build queue, but nothing
/// stops two watcher tasks of this process from entering a transaction on the
/// same git repository at once.
static APPLY_GUARD: Mutex<()> = Mutex::const_new(());

/// Declare `info` as a mount point in `fstab.nix`.
///
/// # Parameters
/// * `info` - the mount to declare. `info.disk_path` is the device written to
///   the configuration (`/dev/disk/by-uuid/<uuid>`; for a LUKS mount, the
///   locked container's UUID), `info.options` the comma-separated option list
///   to write verbatim.
/// * `mapper_device` - the cleartext mapper device file (`/dev/mapper/<name>`)
///   when UDisks2 reports `info` as an unlocked LUKS volume; `None` for a plain
///   device. It is passed on as-is: what the declaration ends up looking like
///   is core-utils' decision, not this module's.
///
/// # Post-conditions
/// `fileSystems."<info.mount_point>"` is declared and a `nixos-rebuild switch`
/// has completed, unless [`crate::dry_run::is_dry_run`] is true — in which case
/// nothing is written. `tpm2` is never requested: it cannot be deduced from an
/// `fstab` entry, and `false` only means "do not add it", so an enrolment
/// already in the configuration survives.
///
/// # Errors
/// [`Error::CoreUtils`] if the blocking task panics or is cancelled, or if the
/// `modulix-core-utils` transaction fails (the configuration is then rolled
/// back by core-utils).
pub(super) async fn mount(info: &MountInfo, mapper_device: Option<&str>) -> Result<(), Error> {
    tracing::info!(
        mount_point = %info.mount_point,
        device = %info.disk_path,
        fs_type = %info.filesystem_type,
        options = %info.options,
        mapper_device = mapper_device.unwrap_or("-"),
        "declaring mount point in the NixOS configuration"
    );

    if crate::dry_run::is_dry_run() {
        return Ok(());
    }

    let (mount_point, device, fs_type) = (
        info.mount_point.clone(),
        info.disk_path.clone(),
        info.filesystem_type.clone(),
    );
    let options = option_list(&info.options);
    let mapper_device = mapper_device.map(str::to_string);

    let _guard = APPLY_GUARD.lock().await;
    tokio::task::spawn_blocking(move || {
        let options: Vec<&str> = options.iter().map(String::as_str).collect();
        let device = match &mapper_device {
            None => MountDevice::Plain { device: &device },
            Some(mapper) => MountDevice::Luks {
                container: &device,
                mapper_device: Some(mapper),
                tpm2: false,
            },
        };

        filesystem::add_mount(
            crate::config_dir::config_dir(),
            &mount_point,
            &device,
            &fs_type,
            &options,
        )
    })
    .await
    .map_err(|err| Error::CoreUtils(err.to_string()))?
    .map_err(|err| Error::CoreUtils(err.to_string()))
}

/// Drop `info`'s mount point from `fstab.nix`.
///
/// # Parameters
/// * `info` - the mount to undeclare; only `info.mount_point` is used, as that
///   is the key of `fileSystems`.
///
/// # Post-conditions
/// `fileSystems."<info.mount_point>"` is gone and a `nixos-rebuild switch` has
/// completed, unless [`crate::dry_run::is_dry_run`] is true. A mount point that
/// was not declared is not an error, but still costs a rebuild (see the
/// module-level limitations).
///
/// # Errors
/// [`Error::CoreUtils`] if the blocking task panics or is cancelled, or if the
/// `modulix-core-utils` transaction fails.
pub(super) async fn unmount(info: &MountInfo) -> Result<(), Error> {
    tracing::info!(
        mount_point = %info.mount_point,
        device = %info.disk_path,
        "dropping mount point from the NixOS configuration"
    );

    if crate::dry_run::is_dry_run() {
        return Ok(());
    }

    let mount_point = info.mount_point.clone();

    let _guard = APPLY_GUARD.lock().await;
    let removed = tokio::task::spawn_blocking(move || {
        filesystem::remove_mount(crate::config_dir::config_dir(), &mount_point)
    })
    .await
    .map_err(|err| Error::CoreUtils(err.to_string()))?
    .map_err(|err| Error::CoreUtils(err.to_string()))?;

    if !removed {
        tracing::info!(mount_point = %info.mount_point, "mount point was not declared");
    }

    Ok(())
}

/// Split an `fstab` `opts` string into the option list `filesystem::add_mount`
/// expects.
///
/// # Parameters
/// * `options` - comma-separated mount options, as UDisks2 reports them
///   verbatim from `/etc/fstab`.
///
/// # Returns
/// One element per non-empty option, in their original order and without
/// deduplication — the user's spelling is written as-is. An empty or
/// comma-only input yields an empty list, which declares the mount point with
/// no `.options` at all.
fn option_list(options: &str) -> Vec<String> {
    options
        .split(',')
        .map(str::trim)
        .filter(|option| !option.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[path = "apply-tests.rs"]
mod tests;
