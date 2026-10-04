//! What an intercepted `Block` configuration method call asks for.
//!
//! Turning the three method calls into one [`Intent`] is pure: it only reads
//! the `(sa{sv})` items the call carries, with no D-Bus round trip, so the
//! whole mapping is unit-testable. Resolving an intent into the device facts
//! needed to report it (UUID, filesystem type, LUKS backing device) is the
//! caller's job, in [`super::monitor`].

use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

use super::mount_info::{self, FstabEntry};

/// One `(sa{sv})` configuration item: its kind (`"fstab"`, `"crypttab"`, ...)
/// and its details.
pub(super) type ConfigItem = (String, HashMap<String, OwnedValue>);

/// What a `Block` configuration method call asks for, once its items are read
/// as `fstab` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Intent {
    /// Configure a mount that was not configured before
    /// (`AddConfigurationItem`).
    Mount(FstabEntry),
    /// Drop a mount's configuration (`RemoveConfigurationItem`).
    Unmount(FstabEntry),
    /// Change the options of a mount, its mount point unchanged
    /// (`UpdateConfigurationItem` with the same `dir`).
    Options {
        /// The entry as it stands, holding the mount point and the old
        /// options.
        current: FstabEntry,
        /// The options replacing `current.options`.
        new_options: String,
    },
    /// Move a mount to another mount point (`UpdateConfigurationItem` with a
    /// different `dir`): an unmount of `old` followed by a mount of `new`.
    Move {
        /// The entry being replaced.
        old: FstabEntry,
        /// The entry replacing it.
        new: FstabEntry,
    },
}

/// Read the `fstab` entry out of a configuration item.
///
/// # Parameters
/// * `item` - one `(sa{sv})` item argument of an intercepted call.
///
/// # Returns
/// `Some(FstabEntry)` when `item` is of kind `"fstab"` and its `dir`/`opts`
/// are readable; `None` for any other kind (e.g. `"crypttab"`, whose details
/// carry `name`/`device`/`options` and no `dir` at all) or an unreadable
/// entry.
fn fstab_entry(item: &ConfigItem) -> Option<FstabEntry> {
    if item.0 != "fstab" {
        return None;
    }

    mount_info::fstab_details(&item.1)
}

/// Build the [`Intent`] a configuration method call expresses.
///
/// # Parameters
/// * `member` - the called method's name. Only
///   `AddConfigurationItem`, `RemoveConfigurationItem` and
///   `UpdateConfigurationItem` map to an intent; every other member of
///   `org.freedesktop.UDisks2.Block` yields `None`.
/// * `items` - the `(sa{sv})` items the call carries, in argument order: one
///   for `Add`/`Remove`, two (old then new) for `Update`.
///
/// # Returns
/// The matching [`Intent`], or `None` when `member` is not a configuration
/// method, when `items` does not hold the number of items that member takes,
/// when an item is not a readable `"fstab"` entry, or when an
/// `UpdateConfigurationItem` changes neither the mount point nor the set of
/// options (see `mount_info::same_options`) and so asks for nothing.
pub(super) fn intent(member: &str, items: &[ConfigItem]) -> Option<Intent> {
    match (member, items) {
        ("AddConfigurationItem", [item]) => Some(Intent::Mount(fstab_entry(item)?)),
        ("RemoveConfigurationItem", [item]) => Some(Intent::Unmount(fstab_entry(item)?)),
        ("UpdateConfigurationItem", [old, new]) => {
            let (old, new) = (fstab_entry(old)?, fstab_entry(new)?);

            if old.mount_point != new.mount_point {
                return Some(Intent::Move { old, new });
            }
            if mount_info::same_options(&old.options, &new.options) {
                return None;
            }

            Some(Intent::Options {
                new_options: new.options,
                current: old,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "intent-tests.rs"]
mod tests;
