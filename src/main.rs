//! mx-daemon: system D-Bus daemon for Modulix OS.
//!
//! Listens to existing D-Bus interfaces (see [`listener`]) and serves its
//! own two interfaces at the same object path: `org.modulix.Daemon` (writes,
//! see [`daemon`] and [`command`]) and `org.modulix.Store1` (reads, see
//! [`store`]).

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
/// an explicit rebuild signal (see [`handle_rebuild_signals`]).
const INDEX_REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Rebuilds the index (if missing/stale) on a timer, so a daemon that never
/// receives SIGHUP still self-heals after a `nixos-rebuild`.
fn spawn_index_refresh_timer() {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(INDEX_REFRESH_INTERVAL);
        ticker.tick().await; // first tick fires immediately; skip it
        loop {
            ticker.tick().await;
            modulix_core_utils::package_index::invalidate_fingerprint();
            modulix_core_utils::package_index::ensure_fresh_in_background().await;
        }
    });
}

/// SIGHUP forces an index rebuild — the daemon's "rebuild signal" (e.g. from
/// a NixOS activation hook after `nixos-rebuild switch`).
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

    // Detached: never awaited, never blocks startup. Until it completes,
    // `NixPackage::search_scored` keeps using its live `nix search` fallback.
    tokio::spawn(modulix_core_utils::package_index::ensure_fresh_in_background());
    spawn_index_refresh_timer();
    spawn_rebuild_signal_handler()?;

    // The connection keeps serving in the background; park forever.
    std::future::pending::<Result<(), Error>>().await
}
