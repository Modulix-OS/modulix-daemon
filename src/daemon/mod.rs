//! The daemon's own D-Bus interface: `org.modulix.Daemon`.
//!
//! Commands are added as methods on the [`Daemon`] interface impl below,
//! each delegating to a [`crate::command::Command`] from
//! [`crate::command::registry`].

/// Well-known bus name the daemon owns.
pub const BUS_NAME: &str = "org.modulix.Daemon";

/// Object path at which the [`Daemon`] interface is served.
pub const OBJECT_PATH: &str = "/org/modulix/Daemon";

/// The `org.modulix.Daemon` interface implementation.
pub struct Daemon;

#[zbus::interface(name = "org.modulix.Daemon")]
impl Daemon {}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
