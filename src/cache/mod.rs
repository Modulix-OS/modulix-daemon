//! Long-lived caches owned by the daemon (see [`crate::store`]).
//!
//! Unlike the old in-process `gnome-software-plugin/backend`, these survive
//! for as long as `mx-daemon` runs — not just one GNOME Software session —
//! and are shared by every client on the machine.

pub mod flight;

pub use flight::FlightCache;
