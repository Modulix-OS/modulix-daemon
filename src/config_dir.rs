//! Runtime override for the Modulix configuration directory.
//!
//! `modulix_core_utils::CONFIG_DIRECTORY` is a compile-time constant that
//! resolves to `$CARGO_MANIFEST_DIR/test/` in debug builds — a sandbox path
//! that does not exist at runtime under Nix. `MX_DAEMON_CONFIG_DIR` lets the
//! deployment pick the directory instead, keeping the release default intact
//! when unset.
//!
//! Neither [`config_dir`] nor [`resolve`] validates that the returned path
//! exists, is a directory, or is a Git repository, and neither creates it —
//! they only compute a string. The daemon (`User=root` under systemd, see
//! the crate's `CLAUDE.md`) needs root privileges to write inside the
//! release default `/etc/modulix-os/`; an `MX_DAEMON_CONFIG_DIR` override
//! pointed at a location writable by an unprivileged user lifts that
//! requirement for that location only.

use std::sync::OnceLock;

/// Directory holding the NixOS configuration git repository the daemon
/// operates on. Always ends with `/`, like `CONFIG_DIRECTORY`.
///
/// # Pre-conditions
/// None; safe to call before any other initialization.
///
/// # Post-conditions
/// The resolution (env lookup + [`resolve`]) runs at most once per process:
/// the result is cached in a static [`OnceLock`] and reused on every
/// subsequent call, so a change to `MX_DAEMON_CONFIG_DIR` after the first
/// call has no effect for the rest of the process lifetime.
///
/// # Returns
/// `$MX_DAEMON_CONFIG_DIR` (with a trailing `/` appended if missing) when
/// that variable is set to a non-empty value, else
/// `modulix_core_utils::CONFIG_DIRECTORY` as-is (already trailing-slashed).
/// The path is returned as given/compiled-in — it is neither validated nor
/// created.
pub fn config_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| resolve(std::env::var("MX_DAEMON_CONFIG_DIR").ok().as_deref()))
}

/// Pure resolution step, split out so it is testable without touching the
/// process environment (`env::set_var` is `unsafe` in edition 2024).
///
/// # Parameters
/// - `from_env`: the value read from `MX_DAEMON_CONFIG_DIR`, if any
///   (`None` when the variable is unset or not valid UTF-8).
///
/// # Pre-conditions
/// None.
///
/// # Post-conditions
/// Performs no filesystem access: does not check that the resulting path
/// exists, is a directory, or is writable, and does not create it.
///
/// # Returns
/// `from_env` when `Some` and non-empty, else
/// `modulix_core_utils::CONFIG_DIRECTORY`; in both cases with exactly one
/// trailing `/` (appended if missing, left as-is if already present).
fn resolve(from_env: Option<&str>) -> String {
    let raw = match from_env {
        Some(s) if !s.is_empty() => s,
        _ => modulix_core_utils::CONFIG_DIRECTORY,
    };
    if raw.ends_with('/') {
        raw.to_string()
    } else {
        format!("{raw}/")
    }
}

#[cfg(test)]
#[path = "config_dir-tests.rs"]
mod tests;
