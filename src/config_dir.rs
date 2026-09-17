//! Runtime override for the Modulix configuration directory.
//!
//! `modulix_core_utils::CONFIG_DIRECTORY` is a compile-time constant that
//! resolves to `$CARGO_MANIFEST_DIR/test/` in debug builds — a sandbox path
//! that does not exist at runtime under Nix. `MX_DAEMON_CONFIG_DIR` lets the
//! deployment pick the directory instead, keeping the release default intact
//! when unset.

use std::sync::OnceLock;

/// Directory holding the NixOS configuration git repository the daemon
/// operates on. Always ends with `/`, like `CONFIG_DIRECTORY`.
pub fn config_dir() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| resolve(std::env::var("MX_DAEMON_CONFIG_DIR").ok().as_deref()))
}

/// Pure resolution step, split out so it is testable without touching the
/// process environment (`env::set_var` is `unsafe` in edition 2024).
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
