//! mx-daemon: system D-Bus daemon for Modulix OS.
//!
//! Listens to existing D-Bus interfaces (see [`listener`]) and serves its
//! own two interfaces at the same object path: `org.modulix.Daemon` (writes,
//! see [`daemon`] and [`command`]) and `org.modulix.Store1` (reads, see
//! [`store`]).
//!
//! # Bus identity
//!
//! The daemon owns the well-known bus name `daemon::BUS_NAME`
//! (`org.modulix.Daemon`) and serves both interfaces at
//! `daemon::OBJECT_PATH` (`/org/modulix/Daemon`) on the *system* bus. Of the
//! two interfaces served there, `org.modulix.Store1` is unprivileged (any
//! caller the D-Bus policy lets reach it may call it — read-only, see
//! [`store`]) and `org.modulix.Daemon` is polkit-gated (every write method
//! checks authorization first — see [`polkit`] and [`daemon`]); the D-Bus
//! policy (`org.modulix.Daemon.conf`) grants both interfaces to every
//! caller, so this split is enforced by the interfaces themselves, not by
//! the bus policy.
//!
//! # Startup sequence
//!
//! [`main`] does, in order: initializes tracing, connects to the system bus
//! and claims `BUS_NAME` while serving both interfaces, spawns one task per
//! entry of [`listener::registry`] (each watches one existing D-Bus
//! interface, e.g. UDisks2), kicks off an unawaited background warm-up of
//! `modulix_core_utils::package_index` (so the first `Store1` query doesn't
//! have to wait for it — it falls back to a live `nix search` until the
//! warm-up completes), starts [`spawn_index_refresh_timer`] (periodic
//! self-healing rebuild) and [`spawn_rebuild_signal_handler`] (SIGHUP-driven
//! rebuild), then parks forever so the connection keeps serving in the
//! background.
//!
//! # Activation
//!
//! Not D-Bus-activated: the repo installs a `dbus-1/system.d` access-control
//! policy (`org.modulix.Daemon.conf`) and a polkit action policy
//! (`org.modulix.daemon.policy`) via `flake.nix`'s `postInstall`, but no
//! `dbus-1/system-services/*.service` activation file, so `dbus-daemon`
//! itself never spawns this binary on demand. It is started instead by a
//! systemd unit (`module.nix`, `systemd.services.mx-daemon`): `Type=dbus`
//! with `BusName = "org.modulix.Daemon"` (systemd waits for the name to be
//! claimed before considering the unit started), `User = "root"`,
//! `wantedBy = [ "multi-user.target" ]` (started at boot, not lazily),
//! `Restart = "on-failure"`.
//!
//! # Configuration surface
//!
//! No CLI arguments are parsed. This binary itself reads a single
//! environment variable, `RUST_LOG` (via [`EnvFilter::try_from_default_env`]
//! in [`main`]), falling back to the `info` level filter when unset or
//! unparsable. `MX_DAEMON_DRY_RUN` and `MX_DAEMON_CONFIG_DIR` also affect
//! this crate's behavior but are read elsewhere (`dry_run`/`config_dir`
//! modules), not by this file.

mod cache;
mod command;
mod config_dir;
mod daemon;
mod dry_run;
mod error;
mod listener;
mod polkit;
mod store;

use std::time::Duration;

use error::Error;
use tracing_subscriber::EnvFilter;

/// How often the package index's nixpkgs fingerprint is force-invalidated so
/// a long-lived daemon eventually notices a `nixos-rebuild` without needing
/// an explicit rebuild signal (see [`spawn_rebuild_signal_handler`]).
///
/// Fixed at 6 hours; not configurable via CLI argument or environment
/// variable.
const INDEX_REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Rebuilds the index (if missing/stale) on a timer, so a daemon that never
/// receives SIGHUP still self-heals after a `nixos-rebuild`.
///
/// # Pre-conditions
///
/// Must be called from within a running Tokio runtime (it spawns a task).
///
/// # Post-conditions
///
/// Spawns a detached background task and returns immediately without
/// waiting for it; the task loops forever, sleeping [`INDEX_REFRESH_INTERVAL`]
/// between iterations, invalidating the package index fingerprint and then
/// triggering a background refresh (`modulix_core_utils::package_index`)
/// each time it wakes. The first tick is consumed immediately on task start
/// so the first real invalidation happens one full interval after startup,
/// not right away.
///
/// # Returns
///
/// Nothing (`()`); the spawned task itself never yields a value and is
/// never joined.
fn spawn_index_refresh_timer() {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INDEX_REFRESH_INTERVAL);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            modulix_core_utils::package_index::invalidate_fingerprint();
            modulix_core_utils::package_index::ensure_fresh_in_background().await;
        }
    });
}

/// SIGHUP forces an index rebuild — the daemon's "rebuild signal" (e.g. from
/// a NixOS activation hook after `nixos-rebuild switch`).
///
/// # Pre-conditions
///
/// Must be called from within a running Tokio runtime (it registers a
/// signal handler and spawns a task).
///
/// # Post-conditions
///
/// On success, a SIGHUP signal stream is registered and a detached task is
/// spawned that awaits it forever: each received SIGHUP invalidates the
/// package index fingerprint and triggers a background refresh
/// (`modulix_core_utils::package_index`). The function itself returns as
/// soon as registration succeeds; it does not wait for any signal.
///
/// # Returns
///
/// `Ok(())` once the signal stream is registered and the task spawned.
///
/// # Errors
///
/// Returns [`Error::Io`] if registering the SIGHUP handler with the OS
/// fails (wraps the underlying `std::io::Error` from
/// `tokio::signal::unix::signal`).
fn spawn_rebuild_signal_handler() -> Result<(), Error> {
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        .map_err(Error::Io)?;
    tokio::spawn(async move {
        while sighup.recv().await.is_some() {
            tracing::info!("SIGHUP received, rebuilding package index");
            modulix_core_utils::package_index::invalidate_fingerprint();
            modulix_core_utils::package_index::ensure_fresh_in_background().await;
        }
    });
    Ok(())
}

/// Entry point: brings up tracing, the D-Bus connection (bus name +
/// interfaces), every listener, and the package-index maintenance tasks,
/// then parks forever.
///
/// # Parameters
///
/// None — no CLI arguments are read.
///
/// # Pre-conditions
///
/// Must run with access to the system D-Bus socket and permission to own
/// [`daemon::BUS_NAME`] there (in production this means running as `root`
/// under the `mx-daemon` systemd unit; see the module docs above).
///
/// # Post-conditions
///
/// On success this function never returns during normal operation: after
/// setup it awaits [`std::future::pending`], so the process keeps running
/// until it is terminated externally (signal, systemd stop) or an early
/// setup step fails.
///
/// # Returns
///
/// `Result<(), Error>` per `#[tokio::main]`'s requirements; in practice only
/// the `Err` arm is ever observed (from a setup failure), since the success
/// path never completes.
///
/// # Errors
///
/// Propagates, via `?`: [`Error::Zbus`] if connecting to the system bus,
/// claiming [`daemon::BUS_NAME`], or registering either interface fails;
/// and [`Error::Io`] if [`spawn_rebuild_signal_handler`] fails to register
/// the SIGHUP handler.
#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let connection = zbus::connection::Builder::system()?
        .name(daemon::BUS_NAME)?
        .serve_at(daemon::OBJECT_PATH, daemon::Daemon::new())?
        .serve_at(daemon::OBJECT_PATH, store::Store)?
        .build()
        .await?;

    let commands = command::registry();
    tracing::info!(count = commands.len(), "registered own-interface commands");

    let listeners = listener::registry();
    tracing::info!(count = listeners.len(), "registered listened interfaces");

    for listener in listeners {
        let connection = connection.clone();
        tokio::spawn(async move {
            if let Err(err) = listener.listen(connection).await {
                tracing::error!(name = listener.name(), %err, "listener failed");
            }
        });
    }

    tokio::spawn(modulix_core_utils::package_index::ensure_fresh_in_background());
    spawn_index_refresh_timer();
    spawn_rebuild_signal_handler()?;

    std::future::pending::<Result<(), Error>>().await
}
