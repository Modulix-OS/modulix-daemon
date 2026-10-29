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

### `modulix-core-utils` comes from GitHub, pinned by tag

`Cargo.toml` depends on `git = "https://github.com/Modulix-OS/modulix-core-utils", tag = "v1.1.1"`, not on a sibling `path`. The old path dependency only worked because `flake.nix` copied the flake input into `$NIX_BUILD_TOP/modulix-core-utils` from `postUnpack`; that hack is gone, and `Cargo.lock` is now the single source of truth for the compiled rev.

**Two pins, one hand-edited number, no silent drift.** The crate is pinned by `Cargo.lock` (what the daemon compiles against) *and* by the flake input (whose packages `module.nix` takes `mx-init` from — that is why the input survived the move). A flake input url cannot read a file, so the tag string genuinely has to be written twice: in `Cargo.toml` and in `flake.nix`. Everything else is derived, and a disagreement is an eval error.

- `flake.nix`'s `coreUtils` binding parses `Cargo.lock` with `builtins.fromTOML`, pulls the rev out of the `git+…#<rev>` source, and compares it with `modulix-core-utils.rev`. Mismatch → `throw` naming both revs and the two commands to run. A lock whose `source` line was eaten by a local `[patch]` throws its own message instead.
- `cargoLock.outputHashes` is keyed `"modulix-core-utils-${coreUtils.version}"` with `coreUtils.narHash`, i.e. the crate version from `Cargo.lock` and the input's `narHash` from `flake.lock`. Nothing to recompute by hand on a bump: a `github:` input's `narHash` is the NAR of the unpacked tree, which is exactly what `rustPlatform`'s `fetchgit` of the same rev produces (verified on v1.0.2, v1.0.3 and v1.1.x). It would only diverge if the repo gained submodules, and then the hash-mismatch error prints the right value.
- Bumping core-utils is therefore: edit the tag in `Cargo.toml` and in `flake.nix`, then `cargo update -p modulix-core-utils` and `nix flake update modulix-core-utils`. Forget either command and `nix build` refuses to start.
- **Local development against a sibling checkout** uses `.cargo/config.toml` (gitignored via `/.cargo/`):
  ```toml
  [patch."https://github.com/Modulix-OS/modulix-core-utils"]
  modulix-core-utils = { path = "../modulix-core-utils" }
  ```
  `nix build` is unaffected (`src = ./.` on a git flake does not see gitignored files). **The trap:** any `cargo` command run with the patch active rewrites `Cargo.lock` — it drops the `source` line *and* pulls in the sibling checkout's uncommitted dependency bumps (that is how `phf`/`rnix`/`rowan` versions the tag does not require can appear). Committing the lock in that state breaks `nix build`, which has no git source left to fetch. Before every commit, `git diff Cargo.lock` must show the `source = "git+…"` line and nothing else; otherwise move `.cargo/` aside, `git checkout Cargo.lock`, re-insert that one line, and confirm with `cargo metadata --locked`.

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

## System updates — two modes, both applied on the spot

`modulix_core_utils::update::update(config_dir, build_command, cores)` is the
only update entry point the library exposes, and it does everything in one
blocking call: `nix flake update` inside its own commit, then the rebuild, and
both only if `flake.lock` actually moved. There is no staging area, no
pre-resolved candidate lockfile and no shutdown unit — all of that was removed
in core-utils v1.1.0 and is not coming back (see the note at the end of this
section for why a "download now, apply later" split is no longer
expressible).

- `Daemon.UpdateSystem(mode)` (`src/command/update.rs`) takes two modes. The
  table is the pure `fn build_command(mode) -> Option<BuildCommand>`, which is
  *also* the mode validation — there is no second `matches!` to keep in sync:
  - `"switch"` → `BuildCommand::Switch`: `nixos-rebuild switch`, **the running
    system is replaced**. This is what GNOME Software's "Update Now" sends:
    `update_mode_for_flags()` in `gnome-software-plugin` reads
    `GS_PLUGIN_UPDATE_APPS_FLAGS_INTERACTIVE`, which every click-driven path
    sets and `gs-update-monitor.c` never does.
  - `"boot"` → `BuildCommand::Boot`: `nixos-rebuild boot`, the running system
    is left alone and the new one becomes the next boot's default. This is the
    mode the plugin's unattended apply job sends.
  Both are capped to half the cores (`background_cores`), so the session in
  progress keeps its share of the machine. Dry run short-circuits both,
  returning the mode's success message.
  `"build"`, `"stage"` and `"apply"` are **gone** and are rejected as unknown
  modes; a client still sending one finds out rather than being silently
  served `"boot"`.
- **The reply does not say whether anything was rebuilt.** `update` returns
  `Ok(())` both when it rebuilt and when every input was already current, so
  `UpdateSystem` answers `"system updated"` / `"system update prepared for
  next boot"` either way. A client that needs the distinction asks
  `Store1.ListOutdatedInputs` again, or `Store1.RebootRequired` after a
  `"switch"`. There is no `"system already up to date"` reply any more.
- `Store1.CheckUpdate() -> b` and `Store1.ListOutdatedInputs(b) -> aa{sv}`
  (`src/store/mod.rs`) are the read side, and they share one probe: the free
  function `outdated_entries(force_refresh)` behind `UPDATE_CACHE`
  (`FlightCache`, 1 h TTL, single-flight). It calls
  `modulix_core_utils::update::outdated_inputs`, which asks `nix flake
  metadata <ref> --json --refresh --no-write-lock-file` once per direct input
  and **writes nothing at all** — not the configuration's `flake.lock`, not a
  scratch file — which is why this stays on the unprivileged read interface.
  `CheckUpdate` is `!outdated_entries(true).await.is_empty()`, so a
  `ListOutdatedInputs` right after it is free and describes exactly what made
  it answer `true`. A `#[interface]` method cannot call another, which is the
  reason `outdated_entries` is a free function rather than a third method.
- **Nothing is kept between a check and an update.** The revisions an
  `UpdateSystem` installs are whatever upstream carries when it runs, not the
  ones a `CheckUpdate` saw. `PENDING_LOCK`, `take_pending_lock`,
  `clear_pending_lock`, `CHECK_GUARD` and `lock_diff_entries` are gone with
  the candidate they existed for.
- `Store1.StagedUpdate` and `Store1.ListStagedInputs` are **removed**: there
  is no staging area to report on, and nothing in mxpkgs,
  `gnome-software-plugin` or `modulix-store-client` ever called them.

**Why there is no build-without-activating mode.** `BuildCommand::Build`
still exists in core-utils, but `update()` hardcodes `UpdateInput::UpdateAll`
and commits the refreshed `flake.lock` as part of the same transaction. A
`"build"` mode would therefore commit the new lock, and the `"switch"` that
followed would find `nix flake update` moving nothing — no commit, no rebuild,
nothing activated. The download and build cost is paid by whichever of the two
real modes runs; the plugin's `NO_APPLY` download step is a no-op on its side.

**`Store1.RebootRequired() -> b` is how a client tells "updated" from
"updated, restart to finish" after a `"switch"`.** A `nixos-rebuild switch`
cannot replace the kernel, the kernel modules or the initrd the machine is
*running*, so a successful switch may still owe a reboot — or may not, if it
moved only userspace. `modulix_core_utils::reboot::reboot_required` settles it
exactly, by comparing `/run/booted-system` with `/run/current-system` on those
three entries; there is no list of "reboot-worthy" package names to maintain
and so no driver it can miss. `systemd` is deliberately not among them:
`switch-to-configuration` re-executes it in place. Three `readlink`s, no
subprocess, no memo — it is a pure read like every other `Store1` method. The
`nix store diff-closures` listing the reply used to carry is gone with the
`(bas)` signature: describing *what* moved is a different question, and no
caller read it for anything but display.

**Invariant: the committed `flake.lock` is the one the running system was built
from.** `update()` commits the refreshed lock and rebuilds in one transaction,
or does neither. An install that happens meanwhile commits with
`UpdateInput::Keep` and cannot drag a refresh in.

**`cache_dir()` is `/var/cache/modulix-os`, and it must stay outside the
configuration repository.** It holds the module and package indexes and
core-utils' ESP probe mount points. The constraint is `nix`'s: it resolves a
bare path by walking its *parents* looking for a git root, so a flake
directory placed under this one is read as a `git+file://` flake restricted to
tracked files rather than as the `path:` flake it is. While the cache lived
under `/etc/modulix-os/.cache` that walk reached the configuration repository,
and nothing untracked written there was visible to `nix`. Listing the
directory in `.git/info/exclude` does not help — excluded is still untracked
as far as `nix` is concerned. The directory is created by `CacheDirectory =
"modulix-os"` on `modulix-daemon.service` (`module.nix`), and `MX_CACHE_DIR`
points core-utils at it. A machine installed before the move keeps an inert
`/etc/modulix-os/.cache`; `rm -rf` it by hand.

`CheckUpdate`, `ListOutdatedInputs` and `RebootRequired` need no polkit action
(they are `Store1` reads) and no `Command` impl, so
`org.modulix.daemon.policy`, `org.modulix.Daemon.conf` and
`command::registry()` are untouched by them.

## Hardware re-detection (`UpdateHardware`)

`Daemon.UpdateHardware()` (`src/command/hardware.rs`) calls
`modulix_core_utils::hardware_config::redetect_all_hardware`, which probes the
**live** machine (GPU slots from sysfs, laptop chassis, fingerprint reader),
regenerates `hardware-configuration.nix` and writes the `mx.hardware.*`
options into `hardware.nix`. It takes no argument: there is nothing to name,
the hardware is whatever is plugged in.

- **Its own polkit action, `auth_admin`.** `polkit::ACTION_HARDWARE`
  (`org.modulix.daemon.hardware`) sits with `ACTION_INSTALL`/`ACTION_REMOVE`,
  not with `ACTION_UPDATE`: the latter is deliberately `allow_active=yes` so a
  background update completes unattended, which is exactly what rewriting
  `hardware.nix` and activating the result must *not* do.
- **Shape is `filesystem::add_mount`'s, not `UpdateSystem`'s.**
  `redetect_all_hardware` is a synchronous `make_group_transaction(…,
  BuildCommand::Switch, UpdateInput::Keep, …)` — both hardware files in one
  commit and one rebuild — so the handler wraps it in
  `tokio::task::spawn_blocking` (the `src/command/lifecycle.rs` pattern) and
  relies on `Daemon::run` for `shutdown::enter` and `rebuild::guard`. It
  replaces the running system like `UpdateSystem("switch")` — and like it does
  not restart the daemon — while `UpdateInput::Keep` keeps a flake refresh out.
- **No "something changed" signal.** The detection is idempotent and a
  topology identical to the files produces no commit and no rebuild, but
  core-utils returns `Ok(())` either way, so the reply is
  `"hardware configuration updated"` in both cases. A client that needs the
  distinction reads `Store1.RebootRequired`. Nothing is invalidated in
  `Daemon::run`: the installed-package list does not change, and there is
  nothing else to invalidate.
- `hardware-config` must stay in the `modulix-core-utils` feature list in
  `Cargo.toml` — it gates the whole module, and it pulls `regex` into
  `Cargo.lock`, which `flake.nix` consumes via `cargoLock.lockFile`.
- The detection writes only what it owns (`laptop`, `has_fingerprint`, the five
  options per GPU slot, the three `nvidia.*BusId`); a user's `nvidia.disable`,
  acceleration and compute settings survive. `lspci` on `PATH` is what names
  each GPU's generation — without it an old NVIDIA card gets the current driver
  branch instead of its legacy one.

## Crash safety (`src/shutdown.rs`, `src/rebuild.rs`)

A configuration transaction is not interruptible: core-utils implements no
`Drop` on its `Transaction` and keeps no journal, so a process killed between
the commit and the end of the rebuild leaves the repository committed ahead of
the running system, files sealed `chattr +i`, and an auto-stash nobody pops.
Since core-utils v1.1.0 there is no repair path either — `staging::repair_after_crash`
and `Transaction::restore_orphan_stashes` are gone, so such a stash has to be
popped by hand. Two things keep that from happening:

- **The unit does not restart itself.** `systemd.services.modulix-daemon` in
  `module.nix` sets `restartIfChanged = false`, `stopIfChanged = false` and
  `KillMode = "process"`. Without them, `switch-to-configuration` restarts the
  daemon during the very activation it started, and the default
  `KillMode=control-group` takes `nixos-rebuild` *and*
  `switch-to-configuration` down with it. A new binary therefore only takes
  effect at the next boot or on an explicit `systemctl restart`.
- **`SIGTERM` drains instead of killing.** `shutdown::enter` refuses new
  transactions once a signal arrived, `wait_drained` waits for the ones in
  flight, then the process exits 0 — within the unit's `TimeoutStopSec`
  (30 min, longer than a rebuild).

**Accepted regression: the rebuild no longer has its own cgroup.** v1.1.0
removed the `systemd-run --collect --wait --pipe --unit=mx-rebuild-<pid>-<n>`
wrapper from `Transaction::rebuild_config`; `transaction.rs` now says a caller
that could be restarted by its own rebuild has to put it in a transient unit
itself, and this daemon does not. So `nixos-rebuild` is a plain child process:
`KillMode = "process"` is what keeps a `systemctl stop modulix-daemon` from
taking it down, and past `TimeoutStopSec` a `SIGKILL` can still interrupt a
transaction with nothing to clean it up afterwards.

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
