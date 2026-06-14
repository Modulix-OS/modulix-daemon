//! D-Bus proxies for the `org.freedesktop.UDisks2` interfaces this listener needs.

use std::collections::HashMap;

use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// `org.freedesktop.UDisks2.Block`: properties shared by every block device.
#[proxy(
    interface = "org.freedesktop.UDisks2.Block",
    default_service = "org.freedesktop.UDisks2"
)]
pub trait Block {
    /// Filesystem type, e.g. `ext4`, `vfat`, or `crypto_LUKS` when locked.
    #[zbus(property)]
    fn id_type(&self) -> zbus::Result<String>;

    /// Filesystem UUID, used to address the underlying disk via `/dev/disk/by-uuid`.
    #[zbus(property, name = "IdUUID")]
    fn id_uuid(&self) -> zbus::Result<String>;

    /// Object path of the locked LUKS device backing this one, or `/` when not encrypted.
    #[zbus(property)]
    fn crypto_backing_device(&self) -> zbus::Result<OwnedObjectPath>;

    /// `/etc/fstab`/`/etc/crypttab`-style entries configured for this device.
    #[zbus(property)]
    fn configuration(&self) -> zbus::Result<Vec<(String, HashMap<String, OwnedValue>)>>;

    /// Device file, e.g. `/dev/sda1` or `/dev/dm-0`.
    #[zbus(property)]
    fn device(&self) -> zbus::Result<Vec<u8>>;

    /// Preferred device file to present to the user, e.g. `/dev/mapper/<name>`.
    #[zbus(property)]
    fn preferred_device(&self) -> zbus::Result<Vec<u8>>;
}
