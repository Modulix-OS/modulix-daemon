//! Crate-wide error type.

use thiserror::Error;

/// Errors that can occur anywhere in the daemon.
#[derive(Debug, Error)]
pub enum Error {
    /// Any error coming from the D-Bus connection or interface registration.
    #[error("D-Bus error: {0}")]
    Zbus(#[from] zbus::Error),
}
