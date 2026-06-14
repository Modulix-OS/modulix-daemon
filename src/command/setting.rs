//! Apply scalar option and list-entry changes, each delegating to its own
//! dedicated library function.
//!
//! Both kinds of setting share the same `(name, value, reset)` shape (see
//! [`Setting`]); only the library function they end up calling differs.

use crate::error::Error;

/// One setting change: `(name, value, reset)`.
///
/// - `reset = false`: set `name` to `value`.
/// - `reset = true`: restore `name` to its default value; `value` is
///   ignored and should be sent as an empty string.
pub type Setting = (String, String, bool);

/// Apply a single scalar option change.
pub(crate) async fn apply_option((name, value, reset): &Setting) -> Result<String, Error> {
    if *reset {
        tracing::info!(option = %name, "resetting option to default");

        #[cfg(not(debug_assertions))]
        println!("reset-option {name}");

        Ok(format!("option {name} reset to default"))
    } else {
        tracing::info!(option = %name, value = %value, "setting option");

        #[cfg(not(debug_assertions))]
        println!("set-option {name} {value}");

        Ok(format!("option {name} set to {value}"))
    }
}

/// Apply a single list-entry change.
pub(crate) async fn apply_list((name, value, reset): &Setting) -> Result<String, Error> {
    if *reset {
        tracing::info!(list = %name, "resetting list to default");

        #[cfg(not(debug_assertions))]
        println!("reset-list {name}");

        Ok(format!("list {name} reset to default"))
    } else {
        tracing::info!(list = %name, value = %value, "setting list entry");

        #[cfg(not(debug_assertions))]
        println!("set-list {name} {value}");

        Ok(format!("list {name} entry set to {value}"))
    }
}

#[cfg(test)]
#[path = "setting-tests.rs"]
mod tests;
