//! Crate-wide error type.

use thiserror::Error;

/// Errors that can occur anywhere in the daemon.
#[derive(Debug, Error)]
pub enum Error {
    /// Any error coming from the D-Bus connection or interface registration.
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),

    /// An error bubbled up from `modulix-core-utils` while performing a
    /// module/package/plugin transaction.
    #[error("core-utils error: {0}")]
    CoreUtils(String),

    /// Failure setting up an OS-level facility (e.g. a signal handler).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Error> for zbus::fdo::Error {
    /// Map a crate error to a D-Bus error reply for own-interface methods.
    fn from(err: Error) -> Self {
        match err {
            Error::Zbus(err) => zbus::fdo::Error::ZBus(err),
            Error::CoreUtils(msg) => zbus::fdo::Error::Failed(msg),
            Error::Io(err) => zbus::fdo::Error::Failed(err.to_string()),
        }
    }
}
