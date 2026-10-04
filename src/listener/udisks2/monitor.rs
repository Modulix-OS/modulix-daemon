//! Listener for the `fstab` configuration method calls of
//! `org.freedesktop.UDisks2.Block`, via D-Bus monitoring.
//!
//! Watching the raw `AddConfigurationItem`/`RemoveConfigurationItem`/
//! `UpdateConfigurationItem` calls rather than the `Block.Configuration`
//! property they change buys three things the property cannot give:
//!
//! * **No feedback loop.** Reporting a change eventually rewrites `fstab.nix`
//!   and runs `nixos-rebuild switch`, whose activation relinks every file of
//!   `/etc` to the store unconditionally — including `/etc/fstab`, which
//!   UDisks2 had replaced with a regular file. That makes `Block.Configuration`
//!   change again, as an effect of our own report. A method call cannot be
//!   produced by a rebuild, so this path is immune by construction;
//!   [`super::recent`] is what protects the property path.
//! * **No coalescing.** A `zbus` `PropertyStream` only keeps the latest value,
//!   so two quick changes are merged. A message stream delivers every call.
//! * **The intent, spelled out.** The call carries `fsname`, `dir`, `type` and
//!   `opts` directly, and `UpdateConfigurationItem` carries both the old and
//!   the new item, so mount/unmount/options-change are told apart without
//!   keeping any per-device baseline.
//!
//! This is the same `BecomeMonitor` mechanism as
//! [`crate::listener::hostname1`].
//!
//! # Only authorised calls are acted upon
//! A monitor sees a call, not its outcome, and
//! `org.freedesktop.udisks2.modify-system-configuration` — the polkit action
//! guarding these three methods — defaults to `auth_admin` on `allow_any`,
//! `allow_inactive` *and* `allow_active`. Acting on the call alone would
//! therefore apply a configuration change for a caller polkit refused. Every
//! intercepted call is instead parked in [`super::pending`] under its serial
//! number and only reported once UDisks2's `method_return` arrives; an `error`
//! reply is logged and dropped.
//!
//! # Two connections
//! `BecomeMonitor` switches a connection to receive-only semantics, so it gets
//! its own [`Connection`] and must not be called on the one the daemon serves
//! `org.modulix.Daemon` on. The shared connection passed to
//! [`Listener::listen`] is still needed, and used, to query the device's
//! `Block` properties (UUID, filesystem type, backing device) when reporting.

use std::collections::HashMap;

use async_trait::async_trait;
use futures_util::StreamExt;
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, MatchRule};

use super::intent::{ConfigItem, Intent, intent};
use super::proxies::BlockProxy;
use super::{Listener, mount_info, pending::PendingCalls, recent};
use crate::error::Error;
use mount_info::FstabEntry;

/// Interface whose method calls are monitored.
const BLOCK_INTERFACE: &str = "org.freedesktop.UDisks2.Block";
/// Service whose replies are monitored, to learn whether an intercepted call
/// was authorised.
const SERVICE: &str = "org.freedesktop.UDisks2";

/// Trailing `a{sv}` options argument every `Block` method takes; never read.
type CallOptions = HashMap<String, OwnedValue>;

/// Listener for `org.freedesktop.UDisks2.Block`'s configuration method calls.
///
/// Stateless: all state lives in the dedicated monitor connection opened by
/// [`Listener::listen`] and in the [`PendingCalls`] created for the duration of
/// that call.
pub struct Udisks2MonitorListener;

#[async_trait]
impl Listener for Udisks2MonitorListener {
    /// Returns `"udisks2-monitor"`, this listener's identifier used in logs.
    fn name(&self) -> &'static str {
        "udisks2-monitor"
    }

    /// Opens a dedicated system-bus connection in D-Bus monitor mode and
    /// reports every *authorised* `fstab` configuration method call made on
    /// `org.freedesktop.UDisks2.Block`.
    ///
    /// # Parameters
    /// * `connection` - the daemon's shared system-bus connection, used to
    ///   query the targeted device's `Block` properties. The monitor itself
    ///   runs on a separate connection (see the module-level docs).
    ///
    /// # Pre-conditions
    /// The process must be allowed to call
    /// `org.freedesktop.DBus.Monitoring.BecomeMonitor` on the system bus (in
    /// practice requires running as root, per the daemon's
    /// `Type=dbus`/`User=root` systemd unit). `org.freedesktop.UDisks2` need
    /// not be running: `BecomeMonitor` only installs match rules.
    ///
    /// # Post-conditions
    /// Runs until the monitor connection's message stream ends. An intercepted
    /// configuration call is parked until its reply arrives: a `method_return`
    /// reports it to the external library (after stamping the device in
    /// [`super::recent`]), an `error` reply only logs it. Calls on items of
    /// another kind than `"fstab"`, other members, undecodable bodies and
    /// updates that change nothing are ignored. A failure to handle one
    /// message is logged and does not stop the loop.
    ///
    /// # Errors
    /// Returns [`Error::Zbus`] if opening the monitor connection, building a
    /// match rule, or the `BecomeMonitor` call fails, and propagates the
    /// underlying `zbus` error if reading a message off the stream fails.
    async fn listen(&self, connection: Connection) -> Result<(), Error> {
        let monitor_conn = Connection::system().await?;

        monitor_conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus.Monitoring"),
                "BecomeMonitor",
                &(match_rules()?, 0u32),
            )
            .await?;

        tracing::info!("udisks2 Block method-call monitor started");

        let mut stream = zbus::MessageStream::from(monitor_conn);
        let mut awaiting = PendingCalls::default();

        while let Some(msg) = stream.next().await {
            let msg = msg?;
            let header = msg.header();

            match header.message_type() {
                Type::MethodCall => {
                    let (Some(member), Some(path)) = (header.member(), header.path()) else {
                        continue;
                    };

                    let Ok(items) = items(member.as_str(), msg.body()) else {
                        continue;
                    };
                    let Some(intent) = intent(member.as_str(), &items) else {
                        continue;
                    };

                    tracing::info!(
                        path = %path,
                        member = member.as_str(),
                        ?intent,
                        "configuration call intercepted, awaiting its reply"
                    );
                    awaiting.record(
                        header.primary().serial_num(),
                        OwnedObjectPath::from(path.to_owned()),
                        intent,
                    );
                }
                Type::MethodReturn => {
                    let Some((path, intent)) = header.reply_serial().and_then(|s| awaiting.take(s))
                    else {
                        continue;
                    };

                    tracing::info!(path = %path, ?intent, "configuration call authorised");
                    if let Err(err) = report(&connection, &path, intent).await {
                        tracing::error!(path = %path, %err, "failed to report configuration call");
                    }
                }
                Type::Error => {
                    let Some((path, intent)) = header.reply_serial().and_then(|s| awaiting.take(s))
                    else {
                        continue;
                    };

                    tracing::info!(
                        path = %path,
                        ?intent,
                        error = header.error_name().map(|e| e.as_str()).unwrap_or("unknown"),
                        "configuration call refused by UDisks2, not reported"
                    );
                }
                Type::Signal => {}
            }
        }

        Ok(())
    }
}

/// Build the match rules `BecomeMonitor` is given.
///
/// # Returns
/// Three rules, in order: the `Block` method calls to intercept, and the
/// `method_return` and `error` replies from [`SERVICE`] that say whether an
/// intercepted call was authorised. A rule carries at most one `member=`, so
/// the member is filtered in code rather than with three call rules.
///
/// # Errors
/// [`Error::Zbus`] if `BLOCK_INTERFACE` is not a valid interface name or
/// `SERVICE` not a valid bus name (both are constants, so in practice never).
fn match_rules() -> Result<Vec<String>, Error> {
    let calls = MatchRule::builder()
        .msg_type(Type::MethodCall)
        .interface(BLOCK_INTERFACE)?
        .build();

    let returns = MatchRule::builder()
        .msg_type(Type::MethodReturn)
        .sender(SERVICE)?
        .build();

    let errors = MatchRule::builder()
        .msg_type(Type::Error)
        .sender(SERVICE)?
        .build();

    Ok(vec![
        calls.to_string(),
        returns.to_string(),
        errors.to_string(),
    ])
}

/// Decode the configuration items a `Block` method call carries.
///
/// # Parameters
/// * `member` - the called method's name; decides how many items the body
///   holds.
/// * `body` - the call's arguments, still encoded.
///
/// # Returns
/// One item for `AddConfigurationItem`/`RemoveConfigurationItem`, two (old
/// then new) for `UpdateConfigurationItem`, and an empty vector for any other
/// member.
///
/// # Errors
/// The underlying `zvariant` error if the body does not match the signature
/// `member` implies. The caller treats that as "not for us" rather than as a
/// failure: a monitor sees malformed and unrelated traffic as a matter of
/// course.
fn items(member: &str, body: zbus::message::Body) -> Result<Vec<ConfigItem>, zbus::Error> {
    match member {
        "AddConfigurationItem" | "RemoveConfigurationItem" => {
            let (item, _) = body.deserialize::<(ConfigItem, CallOptions)>()?;
            Ok(vec![item])
        }
        "UpdateConfigurationItem" => {
            let (old, new, _) = body.deserialize::<(ConfigItem, ConfigItem, CallOptions)>()?;
            Ok(vec![old, new])
        }
        _ => Ok(Vec::new()),
    }
}

/// Report an authorised [`Intent`] for the device at `path` to the external
/// library.
///
/// # Parameters
/// * `connection` - connection used to query the device's `Block` properties.
/// * `path` - object path of the device the call targeted.
/// * `intent` - what the (now authorised) call asked for.
///
/// # Post-conditions
/// `path` is stamped in [`super::recent`] before any report, so the property
/// watcher does not report the same change a second time. An
/// [`Intent::Move`] is reported as an unmount followed by a mount.
///
/// # Errors
/// Any [`Error`] from building the `Block` proxy or from querying the device
/// over D-Bus.
async fn report(
    connection: &Connection,
    path: &OwnedObjectPath,
    intent: Intent,
) -> Result<(), Error> {
    let block = BlockProxy::builder(connection).path(path)?.build().await?;
    recent::record(path);

    match intent {
        Intent::Mount(entry) => {
            let (info, backing_device) = gather(connection, &block, entry).await?;
            mount_info::report_mount(connection, &block, &info, &backing_device).await
        }
        Intent::Unmount(entry) => {
            let (info, _) = gather(connection, &block, entry).await?;
            mount_info::report_unmount(&info).await
        }
        Intent::Options {
            current,
            new_options,
        } => {
            let (info, _) = gather(connection, &block, current).await?;
            mount_info::report_options_changed(&info, &new_options).await
        }
        Intent::Move { old, new } => {
            let (old, _) = gather(connection, &block, old).await?;
            mount_info::report_unmount(&old).await?;

            let (new, backing_device) = gather(connection, &block, new).await?;
            mount_info::report_mount(connection, &block, &new, &backing_device).await
        }
    }
}

/// Resolve `entry` into the [`mount_info::MountInfo`] the external library
/// expects.
///
/// # Parameters
/// * `connection` - connection used to query the backing device of an
///   encrypted device.
/// * `block` - `Block` proxy for the device the call targeted.
/// * `entry` - the `fstab` entry carried by the call.
///
/// # Returns
/// As [`mount_info::gather`]: the mount information, and the device's
/// `CryptoBackingDevice` (`/` when not encrypted).
///
/// # Errors
/// Any [`Error`] from [`mount_info::gather`].
async fn gather(
    connection: &Connection,
    block: &BlockProxy<'_>,
    entry: FstabEntry,
) -> Result<(mount_info::MountInfo, OwnedObjectPath), Error> {
    mount_info::gather(connection, block, entry.mount_point, entry.options).await
}

#[cfg(test)]
#[path = "monitor-tests.rs"]
mod tests;
