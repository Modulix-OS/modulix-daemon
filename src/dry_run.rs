//! Runtime override for whether lifecycle commands (install/uninstall
//! package, module, plugin) actually touch the system.
//!
//! Previously this was a `#[cfg(not(debug_assertions))]` compile-time gate:
//! a debug build could never exercise the real `modulix-core-utils` call
//! without being recompiled in release, and a release build could never be
//! dry-run locally. `MX_DAEMON_DRY_RUN` makes it a runtime choice instead,
//! defaulting to the same behaviour as before (dry in debug, real in
//! release) when unset.

/// `true` → lifecycle commands only log and return a status string, without
/// calling into `modulix-core-utils`. `false` → they perform the real
/// transaction.
pub fn is_dry_run() -> bool {
    match std::env::var("MX_DAEMON_DRY_RUN").as_deref() {
        Ok("1") | Ok("true") => true,
        Ok("0") | Ok("false") => false,
        _ => cfg!(debug_assertions),
    }
}
