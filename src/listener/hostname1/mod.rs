//! Listener for SetPrettyHostname/SetStaticHostname/SetHostname method calls
//! via D-Bus monitoring (org.freedesktop.hostname1 doesn't emit
//! PropertiesChanged when the underlying write fails, e.g. read-only /etc).
//!
//! Watching the raw `SetStaticHostname`/`SetHostname` method calls rather
//! than the `Hostname`/`StaticHostname` properties means a request is
//! detected even when `hostname1` fails to persist it: the property never
//! changes in that case, but the method call was still made on the bus.
//! Each intercepted call is meant to be forwarded to the external library
//! (see the crate-level docs in `crate`), which writes the new hostname
//! into the Modulix NixOS configuration and runs the resulting
//! `nixos-rebuild`, blocking for minutes (same kind of blocking as the
//! daemon's own-interface commands, e.g.
//! `crate::daemon::Daemon::install_package`); that call is currently
//! stubbed out as a `println!` that only runs in release builds, see
//! [`report_hostname_changed`].
//!
//! If `systemd-hostnamed` is not running (or not installed), setting up the
//! D-Bus monitor still succeeds: `BecomeMonitor` only registers a match
//! rule on the bus, it does not require the destination service to exist.
//! In that case [`Hostname1Listener::listen`] simply never observes a
//! matching method call and stays idle for as long as the monitor
//! connection is open — it does not error out or retry.

use futures_util::StreamExt;
use zbus::message::Type;
use zbus::{Connection, MatchRule};

use super::Listener;
use crate::error::Error;

/// Listener implementation for `org.freedesktop.hostname1`.
///
/// Stateless: all state lives in the dedicated monitor connection opened by
/// [`Listener::listen`].
pub struct Hostname1Listener;

#[async_trait::async_trait]
impl Listener for Hostname1Listener {
    /// # Returns
    /// The static string `"hostname1"`.
    fn name(&self) -> &'static str {
        "hostname1"
    }

    /// Opens a dedicated system-bus connection in D-Bus monitor mode and
    /// reports every `SetStaticHostname`/`SetHostname` method call made on
    /// `org.freedesktop.hostname1`.
    ///
    /// # Parameters
    /// * `_` - the daemon's shared system-bus connection, unused: this
    ///   listener opens its own dedicated `monitor_conn` instead, since
    ///   `BecomeMonitor` switches a connection into monitor semantics and
    ///   must not be called on the connection the daemon serves its own
    ///   interfaces on.
    ///
    /// # Pre-conditions
    /// The process must be allowed to call
    /// `org.freedesktop.DBus.Monitoring.BecomeMonitor` on the system bus
    /// (in practice requires running as root, per the daemon's
    /// `Type=dbus`/`User=root` systemd unit).
    ///
    /// # Post-conditions
    /// Runs until `monitor_conn`'s message stream ends or a message fails
    /// to decode. Every intercepted `SetStaticHostname`/`SetHostname` call
    /// logs the requested hostname and calls
    /// [`report_hostname_changed`]; other method calls on the interface are
    /// ignored. If `systemd-hostnamed` is absent, the monitor is still
    /// installed successfully and this loop simply never observes a
    /// matching call (see the module-level docs).
    ///
    /// # Errors
    /// Returns [`Error::Zbus`] if opening the monitor connection, building
    /// the match rule, or the `BecomeMonitor` call fails, and propagates
    /// the underlying `zbus` error if reading a message off the stream
    /// fails.
    async fn listen(&self, _: Connection) -> Result<(), Error> {
        let monitor_conn = Connection::system().await?;

        let rule = MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface("org.freedesktop.hostname1")?
            .build();

        monitor_conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus.Monitoring"),
                "BecomeMonitor",
                &(vec![rule.to_string()], 0u32),
            )
            .await?;

        tracing::info!("hostname1 method-call monitor started");

        let mut stream = zbus::MessageStream::from(monitor_conn);

        while let Some(msg) = stream.next().await {
            let msg = msg?;
            let header = msg.header();

            let Some(member) = header.member() else {
                continue;
            };

            match member.as_str() {
                "SetStaticHostname" => {
                    if let Ok((name, _interactive)) = msg.body().deserialize::<(String, bool)>() {
                        tracing::info!(hostname = %name, "SetStaticHostname intercepted");
                        report_hostname_changed(&name);
                    }
                }
                "SetHostname" => {
                    if let Ok((name, _interactive)) = msg.body().deserialize::<(String, bool)>() {
                        tracing::info!(hostname = %name, "SetHostname intercepted");
                        report_hostname_changed(&name);
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }
}

/// Reports a hostname change intercepted from an `org.freedesktop.hostname1`
/// method call to the external library.
///
/// # Parameters
/// * `hostname` - the hostname requested via `SetStaticHostname` or
///   `SetHostname`, taken verbatim from the intercepted D-Bus call's first
///   argument (no validation is performed here).
///
/// # Post-conditions
/// Always logs the change at info level. In release builds only
/// (`#[cfg(not(debug_assertions))]`), also prints `set-hostname {hostname}`
/// to stdout — the current stand-in for calling the external library that
/// writes the new hostname into the Modulix NixOS configuration and runs
/// the resulting `nixos-rebuild`, which blocks for minutes (see the
/// module-level docs). Debug builds only log the intent and skip that call.
fn report_hostname_changed(hostname: &str) {
    tracing::info!(hostname, "detected pretty hostname change request");

    #[cfg(not(debug_assertions))]
    println!("set-hostname {hostname}");
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
