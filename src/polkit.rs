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
//! `<allow_active>auth_admin_keep</allow_active>` — i.e. only a caller on an
//! *active* local session is even eligible, and that session is prompted
//! for **administrator** credentials (not necessarily the caller's own
//! password) before the action is granted.
//!
//! **Authentication is retained, not re-asked**: the `_keep` suffix makes
//! polkit record a *temporary authorization* once the prompt succeeds, scoped
//! to the subject's session. Every later check of the same action id from that
//! session is then granted without any interaction, until the session ends
//! (or the authorization is revoked explicitly). One prompt per session per
//! action, not one per call — a user installing three packages in a row
//! authenticates once.
//!
//! This lives in the policy file alone: there is deliberately **no polkit JS
//! rule** shipped with the NixOS module to adjust it. An earlier version added
//! one (`AUTH_SELF_KEEP` for `wheel`, `AUTH_ADMIN_KEEP` otherwise) back when
//! the policy still said plain `auth_admin` and the rule was the only source
//! of retention. With `auth_admin_keep` in the defaults it had no effect left
//! on a single-administrator machine — NixOS's `security.polkit.adminIdentities`
//! defaults to `unix-group:wheel`, so an administrator authenticating as
//! "an admin" is already authenticating as themselves — and was dropped rather
//! than kept as a second source of truth to hold in sync. Re-adding one is how
//! you would express a per-group exception, not how you turn retention on.
//!
//! **Who is asked**: [`check`] derives the polkit "subject" from the D-Bus
//! message header (`Subject::new_for_message_header`) — i.e. the identity
//! the bus itself attaches to the sender (uid/pid), not anything the caller
//! supplies in the call payload. This is what the retention above is keyed
//! on, and it comes from the bus, so a caller can neither spoof the subject
//! nor borrow another session's retained authorization. With
//! `AllowUserInteraction` set, polkit may then prompt that session's
//! authentication agent for admin credentials per the `auth_admin_keep`
//! default above — if no temporary authorization is already on record.
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
/// `auth_admin_keep` — one prompt per session, retained afterwards (see the
/// module docs above).
pub const ACTION_INSTALL: &str = "org.modulix.daemon.install";

/// polkit action id for uninstall-shaped methods (`Uninstall*`).
///
/// Matches the `id` attribute of the `<action>` for removals in
/// `org.modulix.daemon.policy`, which requires an active session and
/// `auth_admin_keep` — one prompt per session, retained afterwards (see the
/// module docs above).
pub const ACTION_REMOVE: &str = "org.modulix.daemon.remove";

/// polkit action id for `UpdateSystem`.
///
/// Matches the `id` attribute of its `<action>` in
/// `org.modulix.daemon.policy`, which — unlike [`ACTION_INSTALL`]/
/// [`ACTION_REMOVE`] — grants `allow_active="yes"` with no `auth_admin_keep`
/// prompt at all: a background update prepared by GNOME Software must be able
/// to complete unattended, including in a session that never authenticated
/// (so there would be nothing retained to rely on). See that action's
/// `<description>` for the accepted
/// trade-off (any locally active user can trigger a rebuild, at CPU/disk
/// cost and advancing every flake input, without an admin confirmation).
pub const ACTION_UPDATE: &str = "org.modulix.daemon.update";

/// polkit action id for `UpdateHardware`.
///
/// Matches the `id` attribute of its `<action>` in
/// `org.modulix.daemon.policy`, which — like [`ACTION_INSTALL`]/
/// [`ACTION_REMOVE`] and unlike [`ACTION_UPDATE`] — requires an active
/// session and an `auth_admin_keep` prompt: re-detecting the hardware
/// rewrites `hardware.nix` and activates the result, so it is an
/// administrative change and never happens in a session that has not
/// authenticated. Like the two write actions, the prompt is retained for the
/// rest of that session, so a *second* `UpdateHardware` from the same session
/// does rewrite and activate without asking again.
pub const ACTION_HARDWARE: &str = "org.modulix.daemon.hardware";

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
/// - `action_id`: the polkit action id to check — one of
///   [`ACTION_INSTALL`], [`ACTION_REMOVE`], [`ACTION_UPDATE`] or
///   [`ACTION_HARDWARE`].
///
/// # Pre-conditions
///
/// `connection` must be a live connection to the system bus (the one the
/// polkit authority is expected to be reachable on).
///
/// # Post-conditions
///
/// No daemon or system state is mutated by this call itself; it only queries
/// the polkit authority and, depending on policy, may trigger an interactive
/// authentication prompt on the caller's session (via
/// `CheckAuthorizationFlags::AllowUserInteraction`) before returning. A
/// successful prompt does leave state *inside polkit*: the actions carry
/// `auth_admin_keep`, so polkit records a temporary authorization for the
/// caller's session and subsequent calls return `Ok(())` without prompting
/// (see the module docs).
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
