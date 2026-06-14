# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`mx-daemon` (package `modulix-daemon`) is a Rust **system** DBus daemon for Modulix OS (NixOS target, runs under systemd as `Type=dbus`, `User=root`). It has two jobs:

1. **Listen** to existing DBus interfaces (first: UDisks2) and react to their signals/calls.
2. **Serve** its own interface `org.modulix.Daemon` (first command: install/uninstall a system package by name).

In both cases the actual work is performed by calling an external library owned by the user.

> Bus name is `org.modulix.Daemon` (see `module.nix`, the dbus `.conf` and polkit `.policy` referenced in `flake.nix postInstall`). The verbal spec said `org.Modulix.Daemon` — the deployed name is `org.modulix.Daemon`; use that.

## Build & dev

This is a Nix flake. Do **not** build outside the flake env (needs `pkg-config` + `dbus`).

- `nix develop` — dev shell: `rustc`, `cargo`, `dbus`, and `d-spy` (GUI DBus inspector for testing interfaces).
- `nix build` / `nix build .#mx-daemon` — release build (`release = true`).
- `nix build .#mx-daemon-debug` — debug build.
- Inside `nix develop`:
  - `cargo build` / `cargo build --release`
  - `cargo test` — all tests; `cargo test <name>` — single test.
  - `cargo clippy -- -D warnings` and `cargo fmt` are mandatory before commit (global rule).

NixOS integration: `flake.nix` exposes `nixosModules.mx-daemon`; enable via `services.mx.daemon.enable = true`. `postInstall` installs `org.modulix.Daemon.conf` (dbus policy) and `org.modulix.daemon.policy` (polkit) — these files must exist at repo root for the build to succeed.

## Project-specific conventions

- **Library calls are stubbed as prints (for now).** Every place that would call the user's external library currently does a `println!`/log instead. Each call is **preceded by an info-level log** describing the operation being performed.
- **Library calls run in release only.** Gate the actual library invocation (the print) behind `#[cfg(not(debug_assertions))]` (or equivalent release check). Debug builds log the intent but skip the call. This applies to both the existing-interface handlers and the daemon's own-interface commands.
- **Per-file test files.** Each `src/foo.rs` has a sibling `src/foo-tests.rs` holding its unit tests. Wire it in from `foo.rs` with:
  ```rust
  #[cfg(test)]
  #[path = "foo-tests.rs"]
  mod tests;
  ```
- **Small functions & files.** Decompose and factor aggressively; keep functions and files short. Split rather than grow.
- **Docs in English.** All doc comments / documentation in English.
- Binary must be named `mx-daemon` (`module.nix` calls `${pkg}/bin/mx-daemon`). `Cargo.toml` package is `modulix-daemon`, so a `[[bin]] name = "mx-daemon"` is required.

## Architecture (target design — build toward this)

Genericity via traits so new listened interfaces and new own-interface commands are cheap to add.

- **Listened interfaces** (e.g. UDisks2): one trait abstracting "subscribe to a DBus interface and handle its events". Each interface = one impl. First impl watches `org.freedesktop.UDisks2.Block.Configuration` (the `fstab`/`crypttab` entries UDisks2 re-reads from `/etc/fstab`): a new entry reports a mount, a removed entry an unmount, a changed `dir` an unmount+mount, a changed `opts` (same `dir`) an options change. LUKS partitions are covered the same way once unlocked (the mapper device gets its own `Configuration`). Payload to the library: mount point, disk path (by UUID), filesystem type, mount options (+ mapper/backing device names for LUKS).

> **fstab listener trigger.** The UDisks2 listener reacts to `Block.Configuration` changes (`/etc/fstab` edits as seen by UDisks2), not to actual mount/unmount D-Bus calls — UDisks2 has no generic "mounted/unmounted" signal across filesystems. A configuration change is the proxy for "the user wants this mounted/unmounted/remounted".
- **Own interface** (`org.modulix.Daemon`): commands exposed via zbus's `#[interface]`, each command kept thin and delegating to a handler. First command: install/uninstall a system package given only a package name.
- Use **zbus** as the DBus crate. Prefer `Arc<Mutex<T>>` for shared state (global rule).

Adding work = add a new trait impl (listened interface) or a new interface method + handler (own interface); both funnel into the "info log → release-gated library call" pattern above.
