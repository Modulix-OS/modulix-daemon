//! polkit authorization for `org.modulix.Daemon` write methods.
//!
//! The D-Bus policy (`org.modulix.Daemon.conf`) lets any caller *reach* these
//! methods — access control is entirely delegated to polkit, checked here
//! against the caller identity carried by the message header.
//!
//! # Security model — read this before relying on it
//!
//! Two action ids are checked, one per write shape (see [`ACTION_INSTALL`],
//! [`ACTION_REMOVE`]); both are defined in `org.modulix.daemon.policy` with
//! `<allow_any>no</allow_any>`, `<allow_inactive>no</allow_inactive>`,
//! `<allow_active>auth_admin</allow_active>` — i.e. only a caller on an
//! *active* local session is even eligible, and that session is prompted
//! for **administrator** credentials (not necessarily the caller's own
//! password) before the action is granted.
//!
//! **Who is asked**: [`check`] derives the polkit "subject" from the D-Bus
//! message header (`Subject::new_for_message_header`) — i.e. the identity
//! the bus itself attaches to the sender (uid/pid), not anything the caller
//! supplies in the call payload. With `AllowUserInteraction` set, polkit may
//! then prompt that session's authentication agent for admin credentials
//! per the `auth_admin` default above.
//!
//! **On refusal**: [`check`] returns `Err(Error::CoreUtils(_))`. Every write
//! method in [`crate::daemon`] calls [`check`] before doing anything else
//! and bails out on `Err`, so a refusal (declined prompt, non-admin caller,
//! inactive session, …) means no library call and no state change happen.
//!
//! **When polkit itself is absent** (`org.freedesktop.PolicyKit1` not on the
//! system bus): both `AuthorityProxy::new` and `check_authorization` are
//! themselves D-Bus calls, so they fail with a D-Bus error, which reaches
//! [`check`]'s caller as [`Error::Zbus`] via `?`. The check therefore fails
//! **closed** in that case — no polkit means no writes succeed, not
//! unrestricted writes.
//!
//! **Bypassability**: this module cannot be fooled into authorizing the
//! wrong subject (the subject comes from the bus, not from caller-supplied
//! data). It does *not*, however, enforce that every write method actually
//! calls [`check`] — that discipline lives in [`crate::daemon`], not here; a
//! future method that forgets to call [`check`] would run unauthorized.

use zbus_polkit::policykit1::{AuthorityProxy, CheckAuthorizationFlags, Subject};

use crate::error::Error;

/// polkit action id for install-shaped methods (`Install*`).
///
/// Matches the `id` attribute of the `<action>` for installs in
/// `org.modulix.daemon.policy`, which requires an active session and
/// `auth_admin` (see the module docs above).
pub const ACTION_INSTALL: &str = "org.modulix.daemon.install";

/// polkit action id for uninstall-shaped methods (`Uninstall*`).
///
/// Matches the `id` attribute of the `<action>` for removals in
/// `org.modulix.daemon.policy`, which requires an active session and
/// `auth_admin` (see the module docs above).
pub const ACTION_REMOVE: &str = "org.modulix.daemon.remove";

/// polkit action id for `UpdateSystem`.
///
/// Matches the `id` attribute of its `<action>` in
/// `org.modulix.daemon.policy`, which — unlike [`ACTION_INSTALL`]/
/// [`ACTION_REMOVE`] — grants `allow_active="yes"` with no `auth_admin`
/// prompt: a background update prepared by GNOME Software must be able to
/// complete unattended. See that action's `<description>` for the accepted
/// trade-off (any locally active user can trigger a rebuild, at CPU/disk
/// cost and advancing every flake input, without an admin confirmation).
pub const ACTION_UPDATE: &str = "org.modulix.daemon.update";

/// Checks that the caller identified by `header` is authorized for
/// `action_id`, prompting for authentication (polkit agent) if needed.
///
/// # Parameters
///
/// - `connection`: system-bus connection used to reach the polkit authority
///   service (`org.freedesktop.PolicyKit1`).
/// - `header`: message header of the incoming D-Bus call; supplies the bus's
///   own record of the caller's identity, from which the polkit `Subject`
///   is derived. This is not caller-supplied data — the caller cannot
///   spoof it.
/// - `action_id`: the polkit action id to check — in practice always
///   [`ACTION_INSTALL`] or [`ACTION_REMOVE`].
///
/// # Pre-conditions
///
/// `connection` must be a live connection to the system bus (the one the
/// polkit authority is expected to be reachable on).
///
/// # Post-conditions
///
/// No state is mutated by this call itself; it only queries the polkit
/// authority and, depending on policy, may trigger an interactive
/// authentication prompt on the caller's session (via
/// `CheckAuthorizationFlags::AllowUserInteraction`) before returning.
///
/// # Returns
///
/// `Ok(())` iff polkit reports the subject authorized for `action_id`.
///
/// # Errors
///
/// - [`Error::CoreUtils`] if building the `Subject` from `header` fails, or
///   if polkit reports the subject as *not* authorized for `action_id`.
/// - [`Error::Zbus`] (via `?`, `From<zbus::Error>`) if reaching the polkit
///   authority or issuing the authorization check itself fails — this is
///   also what happens when polkit is not running (see module docs): the
///   check fails closed rather than defaulting to allow.
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
