//! Listener for SetPrettyHostname/SetStaticHostname/SetHostname method calls
//! via D-Bus monitoring (org.freedesktop.hostname1 doesn't emit
//! PropertiesChanged when the underlying write fails, e.g. read-only /etc).

use futures_util::StreamExt;
use zbus::message::Type;
use zbus::{Connection, MatchRule};

use super::Listener;
use crate::error::Error;

pub struct Hostname1Listener;

#[async_trait::async_trait]
impl Listener for Hostname1Listener {
    fn name(&self) -> &'static str {
        "hostname1"
    }

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

fn report_hostname_changed(hostname: &str) {
    tracing::info!(hostname, "detected pretty hostname change request");

    #[cfg(not(debug_assertions))]
    println!("set-hostname {hostname}");
}

#[cfg(test)]
#[path = "mod-tests.rs"]
mod tests;
