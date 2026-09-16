//! polkit authorization for `org.modulix.Daemon` write methods.
//!
//! The D-Bus policy (`org.modulix.Daemon.conf`) lets any caller *reach* these
//! methods — access control is entirely delegated to polkit, checked here
//! against the caller identity carried by the message header.

use zbus_polkit::policykit1::{AuthorityProxy, CheckAuthorizationFlags, Subject};

use crate::error::Error;

/// polkit action id for install-shaped methods (`Install*`).
pub const ACTION_INSTALL: &str = "org.modulix.daemon.install";
/// polkit action id for uninstall-shaped methods (`Uninstall*`).
pub const ACTION_REMOVE: &str = "org.modulix.daemon.remove";

/// Checks that the caller identified by `header` is authorized for
/// `action_id`, prompting for authentication (polkit agent) if needed.
/// Returns [`Error::CoreUtils`] when denied or when the check itself fails.
pub async fn check(
    connection: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    action_id: &str,
) -> Result<(), Error> {
    let subject = Subject::new_for_message_header(header)
        .map_err(|e| Error::CoreUtils(format!("polkit subject: {e}")))?;
    let authority = AuthorityProxy::new(connection).await?;
    let result = authority
        .check_authorization(
            &subject,
            action_id,
            &std::collections::HashMap::new(),
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .await?;

    if result.is_authorized {
        Ok(())
    } else {
        Err(Error::CoreUtils(format!(
            "not authorized for action {action_id}"
        )))
    }
}
