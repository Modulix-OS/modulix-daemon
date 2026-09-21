//! Runtime override for whether lifecycle commands (install/uninstall
//! package, module, plugin) actually touch the system.
//!
//! Previously this was a `#[cfg(not(debug_assertions))]` compile-time gate:
//! a debug build could never exercise the real `modulix-core-utils` call
//! without being recompiled in release, and a release build could never be
//! dry-run locally. `MX_DAEMON_DRY_RUN` makes it a runtime choice instead,
//! defaulting to the same behaviour as before (dry in debug, real in
//! release) when unset.

/// Whether lifecycle commands (install/uninstall package, module, plugin)
/// must skip the real `modulix-core-utils` transaction.
///
/// Resolution order, checked on every call (no caching):
/// 1. `MX_DAEMON_DRY_RUN=1` or `MX_DAEMON_DRY_RUN=true` → dry run (`true`).
/// 2. `MX_DAEMON_DRY_RUN=0` or `MX_DAEMON_DRY_RUN=false` → real run
///    (`false`).
/// 3. Unset, or set to any other value → falls back to the build profile:
///    `true` (dry run) in a debug build, `false` (real run) in a release
///    build.
///
/// # Returns
/// `true` when lifecycle commands must only emit their info-level log and
/// return a synthetic success status string, without calling into
/// `modulix-core-utils` — the D-Bus caller still gets an "installed"/
/// "uninstalled" reply even though nothing on the system changed. `false`
/// when they must additionally run the real `modulix-core-utils`
/// transaction (the actual system-modifying call) before replying.
pub fn is_dry_run() -> bool {
    match std::env::var("MX_DAEMON_DRY_RUN").as_deref() {
        Ok("1") | Ok("true") => true,
        Ok("0") | Ok("false") => false,
        _ => cfg!(debug_assertions),
    }
}
