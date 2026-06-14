//! mx-daemon: system D-Bus daemon for Modulix OS.
//!
//! Listens to existing D-Bus interfaces (see [`listener`]) and serves its
//! own interface `org.modulix.Daemon` (see [`daemon`] and [`command`]).

mod command;
mod daemon;
mod error;
mod listener;

use error::Error;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let connection = zbus::connection::Builder::system()?
        .name(daemon::BUS_NAME)?
        .serve_at(daemon::OBJECT_PATH, daemon::Daemon)?
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

    // The connection keeps serving in the background; park forever.
    std::future::pending::<Result<(), Error>>().await
}
