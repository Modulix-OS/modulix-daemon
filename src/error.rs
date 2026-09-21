//! Crate-wide error type.
//!
//! [`Error`] is converted into [`zbus::fdo::Error`] (see the `From` impl
//! below) whenever an own-interface method returns a `Result<_, Error>` to
//! zbus, so every variant here also documents the D-Bus error name and
//! message the GNOME Software plugin (or any other D-Bus client) receives.

use thiserror::Error;

/// Errors that can occur anywhere in the daemon.
///
/// Every variant is convertible to [`zbus::fdo::Error`] via the `From` impl
/// below, so this is also the set of failure modes exposed on the
/// `org.modulix.Daemon` D-Bus interface.
#[derive(Debug, Error)]
pub enum Error {
    /// Any error coming from the D-Bus connection or interface registration
    /// itself (as opposed to a failure of the operation the interface
    /// method performs), produced via `?` from any fallible `zbus` call.
    ///
    /// # Variants
    /// Wraps the originating [`zbus::Error`].
    ///
    /// Converted to `zbus::fdo::Error::ZBus`, whose wire error name is the
    /// hardcoded `org.freedesktop.zbus.Error` (not a `CoreUtils`/`Failed`
    /// name). The message the client sees is the wrapped `zbus::Error`'s
    /// method-error description when it carries one, otherwise its
    /// `Display` string.
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),

    /// An error bubbled up from `modulix-core-utils` while performing a
    /// module/package/plugin transaction (e.g. a failed lifecycle command,
    /// a polkit authorization denial or check failure, or a module with no
    /// plugin namespace).
    ///
    /// # Variants
    /// Holds the human-readable message to surface to the caller.
    ///
    /// Converted to `zbus::fdo::Error::Failed(msg)`, whose wire error name
    /// is `org.freedesktop.DBus.Error.Failed` and whose message is `msg`
    /// verbatim.
    #[error("core-utils error: {0}")]
    CoreUtils(String),

    /// Failure setting up an OS-level facility (e.g. a signal handler),
    /// produced via `?` from any fallible `std::io` call.
    ///
    /// # Variants
    /// Wraps the originating [`std::io::Error`].
    ///
    /// Converted to `zbus::fdo::Error::Failed(err.to_string())`, whose wire
    /// error name is `org.freedesktop.DBus.Error.Failed` and whose message
    /// is the I/O error's `Display` string.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Error> for zbus::fdo::Error {
    /// Map a crate error to a D-Bus error reply for own-interface methods.
    ///
    /// # Parameters
    /// - `err`: the crate-level error to convert.
    ///
    /// # Returns
    /// - [`Error::Zbus`] becomes `zbus::fdo::Error::ZBus(err)` (wire name
    ///   `org.freedesktop.zbus.Error`).
    /// - [`Error::CoreUtils`] becomes `zbus::fdo::Error::Failed(msg)` (wire
    ///   name `org.freedesktop.DBus.Error.Failed`), message = `msg`.
    /// - [`Error::Io`] becomes `zbus::fdo::Error::Failed(err.to_string())`
    ///   (wire name `org.freedesktop.DBus.Error.Failed`), message = the I/O
    ///   error's `Display` string.
    fn from(err: Error) -> Self {
        match err {
            Error::Zbus(err) => zbus::fdo::Error::ZBus(err),
            Error::CoreUtils(msg) => zbus::fdo::Error::Failed(msg),
            Error::Io(err) => zbus::fdo::Error::Failed(err.to_string()),
        }
    }
}
