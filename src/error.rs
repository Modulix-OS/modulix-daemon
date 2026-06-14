//! Crate-wide error type.

use thiserror::Error;

/// Errors that can occur anywhere in the daemon.
#[derive(Debug, Error)]
pub enum Error {
    /// Any error coming from the D-Bus connection or interface registration.
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),
}

impl From<Error> for zbus::fdo::Error {
    /// Map a crate error to a D-Bus error reply for own-interface methods.
    fn from(err: Error) -> Self {
        match err {
            Error::Zbus(err) => zbus::fdo::Error::ZBus(err),
        }
    }
}
