//! D-Bus proxies for the `org.freedesktop.UDisks2` interfaces this listener needs.
//!
//! Every trait here is a thin `zbus`-generated binding (via `#[zbus::proxy]`):
//! it has no logic of its own, it only turns a Rust method call into the
//! matching D-Bus property-get on the target UDisks2 object. See the
//! `udisks2` module (`mod.rs`) for how these proxies are built (in
//! particular, which object path is passed in) and used.

use std::collections::HashMap;

use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// `org.freedesktop.UDisks2.Block`: properties shared by every block device.
///
/// * Interface: `org.freedesktop.UDisks2.Block`.
/// * Service: `org.freedesktop.UDisks2` (`default_service` below).
/// * Object path: none fixed here — `Block` has no `default_path`, since
///   every block device has its own object path (e.g.
///   `/org/freedesktop/UDisks2/block_devices/sda1`). Callers must supply it
///   explicitly, as `mod.rs` does via `BlockProxy::builder(..).path(path)`.
#[proxy(
    interface = "org.freedesktop.UDisks2.Block",
    default_service = "org.freedesktop.UDisks2"
)]
pub trait Block {
    /// `IdType` property: filesystem type on this block device.
    ///
    /// # Returns
    /// The filesystem type as identified by UDisks2, e.g. `ext4`, `vfat`,
    /// or `crypto_LUKS` when the device is a locked LUKS container; empty
    /// if UDisks2 has not identified one.
    ///
    /// # Errors
    /// Returns a `zbus::Error` if the underlying D-Bus property-get call
    /// fails (e.g. the object is gone or the connection is closed).
    #[zbus(property)]
    fn id_type(&self) -> zbus::Result<String>;

    /// `IdUUID` property: filesystem UUID of this block device, used to
    /// address the underlying disk via `/dev/disk/by-uuid`.
    ///
    /// # Returns
    /// The filesystem UUID as reported by UDisks2, or an empty string if
    /// none is set.
    ///
    /// # Errors
    /// Returns a `zbus::Error` if the underlying D-Bus property-get call
    /// fails.
    #[zbus(property, name = "IdUUID")]
    fn id_uuid(&self) -> zbus::Result<String>;

    /// `CryptoBackingDevice` property: object path of the locked LUKS
    /// device backing this one.
    ///
    /// # Returns
    /// The backing device's object path, or `/` (the D-Bus root object
    /// path) when this block device is not an unlocked LUKS cleartext
    /// device.
    ///
    /// # Errors
    /// Returns a `zbus::Error` if the underlying D-Bus property-get call
    /// fails.
    #[zbus(property)]
    fn crypto_backing_device(&self) -> zbus::Result<OwnedObjectPath>;

    /// `Configuration` property: `/etc/fstab`/`/etc/crypttab`-style entries
    /// UDisks2 has read back for this device.
    ///
    /// # Returns
    /// One `(type, details)` pair per configured entry — `type` is e.g.
    /// `"fstab"` or `"crypttab"`, `details` holds the entry's fields (e.g.
    /// `dir`, `opts`) as UDisks2 exposes them. Empty when the device has no
    /// configuration entry.
    ///
    /// # Errors
    /// Returns a `zbus::Error` if the underlying D-Bus property-get call
    /// fails.
    #[zbus(property)]
    fn configuration(&self) -> zbus::Result<Vec<(String, HashMap<String, OwnedValue>)>>;

    /// `Device` property: device file for this block device.
    ///
    /// # Returns
    /// The device path as raw bytes, UDisks2's wire format for filesystem
    /// paths, e.g. `/dev/sda1` or `/dev/dm-0`.
    ///
    /// # Errors
    /// Returns a `zbus::Error` if the underlying D-Bus property-get call
    /// fails.
    #[zbus(property)]
    fn device(&self) -> zbus::Result<Vec<u8>>;

    /// `PreferredDevice` property: device file UDisks2 recommends
    /// presenting to the user for this block device.
    ///
    /// # Returns
    /// The preferred device path as raw bytes, e.g. `/dev/mapper/<name>`
    /// for a mapped LUKS device.
    ///
    /// # Errors
    /// Returns a `zbus::Error` if the underlying D-Bus property-get call
    /// fails.
    #[zbus(property)]
    fn preferred_device(&self) -> zbus::Result<Vec<u8>>;
}
