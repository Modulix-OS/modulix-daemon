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

- **Every library call is preceded by an info-level log** describing the operation being performed. Some handlers still stub the call itself as a `println!` (see `src/command/setting.rs`); the install/uninstall commands, the system update and the UDisks2 mount path call `modulix-core-utils` for real.
- **Dry run is a runtime choice, not a build profile.** Gate the actual library invocation behind `crate::dry_run::is_dry_run()` (`src/dry_run.rs`), driven by `MX_DAEMON_DRY_RUN` and defaulting to the build profile when unset. The old `#[cfg(not(debug_assertions))]` gate is gone: it made the debug build unable to exercise the real path, which is exactly what `nix build .#mx-daemon-test` needs to do.
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

- **Listened interfaces** (e.g. UDisks2): one trait abstracting "subscribe to a DBus interface and handle its events". Each interface = one impl. UDisks2 has **two** impls, see below. The property one watches `org.freedesktop.UDisks2.Block.Configuration` (the `fstab`/`crypttab` entries UDisks2 re-reads from `/etc/fstab`): a new entry reports a mount, a removed entry an unmount, a changed `dir` an unmount+mount, a changed `opts` (same `dir`) an options change. LUKS partitions are covered the same way once unlocked (the mapper device gets its own `Configuration`). Payload to the library: mount point, disk path (by UUID), filesystem type, mount options (+ mapper/backing device names for LUKS).

> **fstab listener trigger — the method call is the primary one.** UDisks2 has no generic "mounted/unmounted" signal, so the trigger is the `fstab` configuration change. There are two ways to observe it and the daemon uses both, `Udisks2MonitorListener` first:
>
> 1. **`Block` method calls** (`src/listener/udisks2/monitor.rs`): a `BecomeMonitor` match rule on `AddConfigurationItem`/`RemoveConfigurationItem`/`UpdateConfigurationItem`. This is the primary path. It sees the request itself, is not subject to the property stream's coalescing, carries the old *and* new item on an update (so no baseline state is needed), and — decisively — **cannot be triggered by our own `nixos-rebuild`**.
> 2. **The `Block.Configuration` property** (`src/listener/udisks2/mod.rs`): covers an `/etc/fstab` change made outside D-Bus and the state of a device as it appears.
>
> Three facts to keep in mind when touching this (all verified, do not re-derive):
>
> - **The write succeeds.** `/etc/fstab` is a symlink to `/etc/static/fstab` → the store, but `/etc` is writable by root and `udisksd` uses `g_file_set_contents` (temp file + `rename`), which replaces the symlink with a regular file. Both paths therefore fire on a successful call.
> - **Feedback loop.** `nixos-rebuild switch` activation (`setup-etc.pl`) relinks every `/etc` file to the store unconditionally, so our own write makes `Block.Configuration` change again. `src/listener/udisks2/recent.rs` stamps a device before the monitor reports and the property watcher skips the echo; its window outlasts a rebuild.
> - **polkit refuses by default.** `org.freedesktop.udisks2.modify-system-configuration` is `auth_admin` on `allow_any`/`allow_inactive`/`allow_active`. **A monitor sees the call, not its outcome**, so acting on the call alone would apply a change polkit refused. `src/listener/udisks2/pending.rs` parks each call by serial and only reports on `method_return`; an `error` reply is dropped. Never bypass this. A test from a plain terminal needs `pkttyagent --process $$ &` or it will be denied — the most likely reason the listener looks silent.
- **Listened interfaces** (hostname1): monitors the `SetStaticHostname`/`SetHostname` **method calls** (`BecomeMonitor` on a dedicated connection), not the `Hostname` property — `hostname1` emits no `PropertiesChanged` when its write under `/etc` fails. On a call, reports the new hostname to the external library. Note it does **not** yet correlate the reply the way `udisks2/pending.rs` does, so it acts on unauthorised attempts too.
- **Own interface** (`org.modulix.Daemon`): commands exposed via zbus's `#[interface]`, each command kept thin and delegating to a handler. First command: install/uninstall a system package given only a package name.
- Use **zbus** as the DBus crate. Prefer `Arc<Mutex<T>>` for shared state (global rule).

Adding work = add a new trait impl (listened interface) or a new interface method + handler (own interface); both funnel into the "info log → dry-run-gated library call" pattern above.

## System updates — staged, applied at shutdown or on demand

**An automatic update is never applied while the machine is in use; a manual
one is.** The split is by *who asked*, not by what is possible: a background
update is only ever resolved and pre-built, and the `nixos-rebuild boot` that
makes it the next boot's default runs at shutdown, so it takes effect on
restart — whereas `UpdateSystem("switch")`, the "Update Now" a user clicked,
activates the pre-built system immediately. No `UpdateSystem` mode writes a
bootloader entry while the session runs except the explicit administrative
`"apply"`. The mechanism lives in `modulix_core_utils::staging`; this daemon
drives it.

- `Store1.CheckUpdate() -> b` (`src/store/mod.rs`) calls
  `modulix_core_utils::update::check_update`, which runs a full
  `nix flake update --output-lock-file <scratch>` — **nothing in the config
  directory is written**, so this stays on the unprivileged read interface. The
  resulting candidate `flake.lock` is parked in `PENDING_LOCK`, a plain
  `Mutex<Option<String>>`: no TTL, take-once, overwritten by the next check.
  Concurrent callers queue behind `CHECK_GUARD` (a `tokio::sync::Mutex`).
  `PENDING_LOCK`'s only consumer is now `staging::stage`, which stages that
  candidate instead of probing a second time; from there it lives on disk,
  which is what survives a daemon restart.
- The same call refills `UPDATE_CACHE` from `update::diff_locks`, a purely
  local diff of the two lockfiles. So `ListOutdatedInputs` right after is free
  and consistent by construction with what an update would apply.
- `Store1.StagedUpdate() -> (bbt)` and `Store1.ListStagedInputs() -> aa{sv}`
  are the read side of the staging area: `(staged, built, created_at)` and the
  per-input rows of what the next boot will carry. Pure reads, no probe — this
  is what a "restart to finish updating" prompt reads, since an up-to-date
  system and one waiting for a reboot look identical to `ListOutdatedInputs`.
- `Daemon.UpdateSystem(mode)` (`src/command/update.rs`), five modes. The mode
  table is the pure `fn steps(mode) -> Option<Steps>`, which is *also* the mode
  validation — there is no second `matches!` to keep in sync:
  - `"build"` / `"stage"` → `staging::stage_update`: resolve + `nixos-rebuild
    build`, nothing applied. Half the cores.
  - `"boot"` → `staging::stage_update` too, and nothing else. The
    `nixos-rebuild boot` that promotes the staged system is
    `mx-apply-update.service`'s job at shutdown and is never run from the
    daemon, so `"boot"` does the same *work* as `"build"` and differs only in
    its reply — which is what sends GNOME Software's row to
    `PENDING_INSTALL`. This is the mode the plugin's **unattended** apply job
    sends; a click sends `"switch"` (see below).
  - `"switch"` → stage if needed, then `staging::apply_staged_with(…,
    Activation::Switch)`: `nixos-rebuild switch`, **the running system is
    replaced**. The manual path, and the only mode that does this. This is what
    GNOME Software's "Update Now" sends: `update_mode_for_flags()` in
    `gnome-software-plugin` reads `GS_PLUGIN_UPDATE_APPS_FLAGS_INTERACTIVE`,
    which every click-driven path sets and `gs-update-monitor.c` never does.
  - `"apply"` → `Activation::Boot` only, no staging. Administrative escape
    hatch that promotes for the next boot by hand; the normal path is the
    shutdown unit.
  Dry run short-circuits every mode *including the probe*, returning the mode's
  success message.
- **The promotion for the next boot only ever happens at shutdown.**
  `mx-apply-update.service` (mxpkgs) runs core-utils' `mx-apply-update` binary
  before `shutdown.target`. That binary calls `apply_staged`, which is
  `apply_staged_with(…, Activation::Boot)` under its own name — the shutdown
  path cannot pick `Switch` by accident, and no `UpdateSystem` mode but the
  explicit `"apply"` can pick `Boot`. A machine that loses power instead of
  shutting down cleanly never runs it: the staged update stays on disk and the
  next clean shutdown applies it.

`Activation` is a two-variant public enum in `modulix_core_utils::staging`, not
the crate's internal `BuildCommand`. `BuildCommand` has six variants, four of
which are meaningless or destructive here (`Install` is
`nixos-install --root /mnt`), and it is not public anyway — `core-utils`'
`src/lib.rs` declares `mod core;` privately. Keeping the narrow type makes the
bad choices unrepresentable instead of rejected at run time.

**Why a `"switch"` does not corrupt the transaction it runs from.**
`switch-to-configuration` would normally restart a unit whose definition
changed — here, killing the daemon mid-transaction. Three facts, all verified,
keep that from happening and should not be re-derived:

- the unit sets `restartIfChanged = false` and `stopIfChanged = false`
  (mxpkgs `modulixos/modulix-daemon/default.nix`), so the activation leaves it
  alone;
- nixpkgs gives `dbus.service` `reloadIfChanged = true`
  (`nixos/modules/services/system/dbus.nix`, "Don't restart dbus-daemon. Bad
  things tend to happen if we do."), so the bus is reloaded and the daemon
  never loses its connection;
- `polkit` *is* restarted (`restartTriggers = [ config.system.path ]`) and so
  is `udisks2`, both harmlessly: `src/polkit.rs` builds its `AuthorityProxy`
  per call, and the UDisks2 listeners' `BecomeMonitor` match rules live on the
  bus rather than on a connection to `udisksd`, while `Watchers::spawn`
  deduplicates a live watcher to preserve its baseline.

The consequence to live with: `restartIfChanged = false` means an update
carrying a new daemon keeps being served by the **old** binary until the next
boot or an explicit `systemctl restart`. A client must not assume a fresh
version's methods exist right after a `"switch"`.

**Invariant: the committed `flake.lock` is the one the running system was built
from.** The staged candidate lives under `cache_dir()/pending-update/`, never in
the git tree, so an install that happens while an update waits commits with
`UpdateInput::Keep` and cannot drag the update in. `apply_staged_with` promotes
the candidate (commit without rebuild) only after the new system is built and
either registered as next boot's or activated.

**`cache_dir()` is `/var/cache/modulix-os`, and it must stay outside the
configuration repository.** It used to be `/etc/modulix-os/.cache`, which broke
every staged build: `nixos-rebuild --flake` is pointed at the bare path of the
`.git`-less staged copy, and `nix` resolves a bare path by walking its
*parents* looking for a git root. That walk reached `/etc/modulix-os/.git`, the
copy was read as a `git+file://` flake restricted to tracked files, and the
build died with `Path '.cache/pending-update/config/flake.nix' … is not tracked
by Git`. Listing the directory in `.git/info/exclude` does not help — excluded
is still untracked as far as `nix` is concerned. Outside any repository the same
bare path is read as a `path:` flake, which is what
`modulix_core_utils::staging` relies on. The directory is created by
`CacheDirectory = "modulix-os"` on both `modulix-daemon.service` and
`mx-apply-update.service` (mxpkgs), and `MX_CACHE_DIR` points core-utils at it.
A machine installed before the move keeps an inert `/etc/modulix-os/.cache`,
still excluded; `rm -rf` it by hand.

`CheckUpdate`, `StagedUpdate` and `ListStagedInputs` need no polkit action (they
are `Store1` reads) and no `Command` impl, so `org.modulix.daemon.policy`,
`org.modulix.Daemon.conf` and `command::registry()` are untouched by them.

## Crash safety (`src/shutdown.rs`, `src/rebuild.rs`)

A configuration transaction is not interruptible: core-utils implements no
`Drop` on its `Transaction` and keeps no journal, so a process killed between
the commit and the end of the rebuild leaves the repository committed ahead of
the running system, files sealed `chattr +i`, and an auto-stash nobody pops.
Three things keep that from happening:

- **The unit does not restart itself.** `systemd.services.modulix-daemon` in
  mxpkgs sets `restartIfChanged = false`, `stopIfChanged = false` and
  `KillMode = "process"`. Without them, `switch-to-configuration` restarts the
  daemon during the very activation it started, and the default
  `KillMode=control-group` takes `nixos-rebuild` *and*
  `switch-to-configuration` down with it. A new binary therefore only takes
  effect at the next boot or on an explicit `systemctl restart`.
- **The rebuild lives in its own cgroup.** `Transaction::rebuild_config` wraps
  the command in `systemd-run --collect --wait --pipe --unit=mx-rebuild-<pid>-<n>`
  when `INVOCATION_ID` is set, so it survives any stop of the daemon unit. That
  is also why the build scratch moved from `/tmp` to `cache_dir()`: with
  `PrivateTmp`, a transient unit does not see the daemon's `/tmp`.
- **`SIGTERM` drains instead of killing.** `shutdown::enter` refuses new
  transactions once a signal arrived, `wait_drained` waits for the ones in
  flight, then the process exits 0 — within the unit's `TimeoutStopSec`.
  `main` calls `staging::repair_after_crash` before serving, which pops the
  auto-stash a previous `SIGKILL` left behind.

`rebuild::guard()` is the single lock every configuration transaction of this
process takes: `Daemon::run` holds it for every command, and
`listener/udisks2/apply.rs` for every mount change. It is **not reentrant** —
code reached from `Daemon::run` must not take it again.

## Mount changes (`fstab.nix` write path)

`src/listener/udisks2/apply.rs` is the write half of the UDisks2 listeners:
`mount_info`'s `report_*` decide *what* happened, `apply` makes it stick.

- `apply::mount` → `filesystem::add_mount`, `apply::unmount` →
  `filesystem::remove_mount`, both on `tokio::task::spawn_blocking` (the
  core-utils API is synchronous) and behind `rebuild::guard()`, the lock every
  configuration transaction of this process takes — two concurrent rebuilds
  would fight over the same git tree, and the own-interface commands reach that
  tree too. Each one also takes `shutdown::enter()`, so a mount change arriving
  during termination is refused instead of started and killed.
- An options change is a **re-declaration**: `add_mount` resets `.options`
  before writing, so passing the new list is enough. Options are written
  verbatim, in the user's order, not normalised.

**No LUKS logic in this repo.** The boundary is: UDisks2 knows the *facts*
(`CryptoBackingDevice` says it is an unlocked LUKS volume, `IdUUID` of the
backing device is the container, `PreferredDevice` is the mapper in use), and
`mount_info` reads them because that needs `zbus`, which core-utils does not
have. Everything that *follows* from those facts is
`filesystem::MountDevice`'s: the mapper name to declare, the
`boot.initrd.luks.devices` entry, the TPM2 attribute, and dropping that entry
again on removal. `apply::mount` therefore only builds
`MountDevice::Plain { device }` or
`MountDevice::Luks { container, mapper_device, tpm2: false }` and hands it over.
`grep -rn "LuksEntry\|default_luks_name\|mapper_name" src/` must stay empty.

`tpm2: false` is safe to pass always: core-utils never resets
`crypttabExtraOpts`, so `false` means "do not add it", not "remove it", and an
existing enrolment survives. An `fstab` entry could not tell us anyway.

Two `modulix-core-utils` limitations this path lives with (documented in
`apply.rs`, not worked around):

1. **One rebuild per entry.** Only the transactional wrappers are public (the
   `*_no_transaction` ones take a `NixFile`, which is not), so a burst of mount
   changes cannot share a rebuild.
2. **No read API.** No `list_entries`/`get_entry`, so the daemon cannot know
   whether what it is about to write is already there; `remove_mount` rebuilds
   even when it removed nothing.

Fixing either belongs in `modulix-core-utils`, not here.
