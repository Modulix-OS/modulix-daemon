//! Listened D-Bus interfaces.
//!
//! A [`Listener`] subscribes to signals/calls on an existing D-Bus interface
//! (e.g. UDisks2) and reacts to them by calling into the user's external
//! library. Each interface to watch gets its own module implementing this
//! trait; [`registry`] lists every implementation that should run.
//!
//! # Startup, ordering and failure handling
//!
//! `main` calls [`registry`] exactly once at startup and, for every
//! returned listener, spawns [`Listener::listen`] on its own `tokio::spawn`
//! task, passing it a clone of the daemon's shared system-bus
//! [`Connection`] (the same connection the daemon serves its own
//! `org.modulix.Daemon`/`org.modulix.Store1` interfaces on). All listeners
//! therefore run concurrently and independently of one another for the
//! whole lifetime of the process; [`registry`]'s return order only decides
//! the order tasks are spawned in, not any ordering guarantee between them.
//!
//! A listener failing (its `listen` future returning `Err`) is **not**
//! fatal to the daemon: the spawning task only logs the error together with
//! [`Listener::name`] and then exits. The daemon keeps running with that
//! one listener stopped while every other listener, and the daemon's own
//! served interfaces, stay unaffected; a stopped listener is not restarted.

use async_trait::async_trait;
use zbus::Connection;

use crate::error::Error;

mod hostname1;
mod udisks2;

/// A single listened D-Bus interface.
///
/// One implementation per interface to watch; see the module-level docs and
/// [`registry`] for how implementations are started, run and what happens
/// if [`Listener::listen`] returns an error.
#[async_trait]
pub trait Listener: Send + Sync {
    /// Human-readable name, used in logs.
    ///
    /// # Returns
    /// A static string identifying this listener, used to tag its log
    /// lines — in particular the "listener failed" error `main` logs when
    /// [`Listener::listen`] returns `Err`.
    fn name(&self) -> &'static str;

    /// Subscribe to the interface and react to its events until the
    /// connection is closed or an unrecoverable error occurs.
    ///
    /// # Parameters
    /// * `connection` - the daemon's shared system-bus connection, cloned
    ///   once per listener by `main` before spawning this future.
    ///
    /// # Post-conditions
    /// Runs for as long as the listener keeps observing/handling events;
    /// only returns once it stops doing so, either because the underlying
    /// D-Bus connection closed or because it hit an unrecoverable error.
    ///
    /// # Errors
    /// Implementations return `Err` for any unrecoverable failure. A
    /// returned error is not propagated to the rest of the daemon: `main`
    /// only logs it (see the module-level docs) and moves on.
    async fn listen(&self, connection: Connection) -> Result<(), Error>;
}

/// All listened interfaces.
///
/// Add a new implementation here to start watching another D-Bus interface.
///
/// # Returns
/// One boxed [`Listener`] per implementation to run. `main` spawns each
/// entry's [`Listener::listen`] on its own task immediately after calling
/// this function, in the order the entries appear here; that order affects
/// only the spawn order, not how the listeners run (all concurrently and
/// independently — see the module-level docs).
pub fn registry() -> Vec<Box<dyn Listener>> {
    vec![
        Box::new(udisks2::Udisks2Listener),
        Box::new(hostname1::Hostname1Listener),
    ]
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
