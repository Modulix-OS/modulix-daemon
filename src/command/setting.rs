//! Apply scalar option and list-entry changes, each delegating to its own
//! dedicated library function.
//!
//! Both kinds of setting share the same `(name, value, reset)` shape (see
//! [`Setting`]); only the library function they end up calling differs.
//!
//! Neither [`apply_option`] nor [`apply_list`] has its own D-Bus method or
//! goes through the [`super::Command`] registry: both are called directly
//! from the single `SetOptions` D-Bus method
//! ([`crate::daemon::Daemon::set_options`]), once per entry of its `options`
//! and `lists` arguments respectively.
//!
//! Unlike the install/uninstall commands in this crate, the actual
//! `modulix-core-utils` library call is **not implemented yet** here: each
//! branch below only `println!`s a stand-in line describing the intended
//! call, and neither function triggers a Nix file edit, a git transaction or
//! a `nixos-rebuild`. That stand-in is also gated differently from the rest
//! of the crate - via the compile-time `#[cfg(not(debug_assertions))]`
//! (release-only), not the runtime [`crate::dry_run::is_dry_run`] check
//! every other command in this module uses.

use crate::error::Error;

/// One setting change: `(name, value, reset)`.
///
/// # Fields
/// * `.0` (`name`) - the option/list key being changed.
/// * `.1` (`value`) - the value to set. Ignored when `.2` is `true`; callers
///   should send an empty string in that case.
/// * `.2` (`reset`) - if `true`, restore `.0` to its default value instead
///   of setting it to `.1`.
///
/// - `reset = false`: set `name` to `value`.
/// - `reset = true`: restore `name` to its default value; `value` is
///   ignored and should be sent as an empty string.
pub type Setting = (String, String, bool);

/// Apply a single scalar option change, as one entry of the `SetOptions`
/// D-Bus method's `options` argument.
///
/// # Parameters
/// * `(name, value, reset)` - see [`Setting`].
///
/// # Post-conditions
/// In a release build (`#[cfg(not(debug_assertions))]`), prints
/// `"reset-option {name}"` (when `reset`) or `"set-option {name} {value}"`
/// (otherwise) as a stand-in for the not-yet-implemented
/// `modulix-core-utils` call; in a debug build nothing beyond the
/// `tracing::info!` call happens. No Nix file, git transaction or
/// `nixos-rebuild` is touched by either path today.
///
/// # Returns
/// `"option {name} reset to default"` when `reset` is `true`, otherwise
/// `"option {name} set to {value}"`.
///
/// # Errors
/// Currently infallible (`Ok` in every case); the `Result` return type
/// matches the real library call this is standing in for.
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

/// Apply a single list-entry change, as one entry of the `SetOptions` D-Bus
/// method's `lists` argument.
///
/// # Parameters
/// * `(name, value, reset)` - see [`Setting`]; here `name` identifies the
///   list and `value` the entry being set/added.
///
/// # Post-conditions
/// In a release build (`#[cfg(not(debug_assertions))]`), prints
/// `"reset-list {name}"` (when `reset`) or `"set-list {name} {value}"`
/// (otherwise) as a stand-in for the not-yet-implemented
/// `modulix-core-utils` call; in a debug build nothing beyond the
/// `tracing::info!` call happens. No Nix file, git transaction or
/// `nixos-rebuild` is touched by either path today.
///
/// # Returns
/// `"list {name} reset to default"` when `reset` is `true`, otherwise
/// `"list {name} entry set to {value}"`.
///
/// # Errors
/// Currently infallible (`Ok` in every case); the `Result` return type
/// matches the real library call this is standing in for.
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
