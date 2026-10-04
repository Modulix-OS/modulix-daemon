//! Intercepted calls waiting for UDisks2's reply.
//!
//! A D-Bus monitor sees a method call, not its outcome. Acting on the call
//! alone would mean applying a configuration change for a caller polkit just
//! refused: `org.freedesktop.udisks2.modify-system-configuration`, the action
//! guarding the `fstab` configuration methods, defaults to `auth_admin` on
//! `allow_any`, `allow_inactive` **and** `allow_active`, so a denied call is
//! the normal outcome of an unauthenticated attempt, not an edge case.
//!
//! [`super::monitor`] therefore parks each intercepted call here under its
//! serial number and only acts once it sees the matching `method_return`; a
//! matching `error` reply drops the entry without acting.
//!
//! # Bounded
//! An authentication prompt keeps a call pending for as long as the user takes
//! to type a password, so [`PENDING_TTL`] is generous. A reply that never comes
//! (caller disconnected, UDisks2 killed) would otherwise leak, hence the
//! [`MAX_PENDING`] cap: inserting past it drops the oldest entry. Both are
//! enforced lazily, on insertion — no timer task.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use zbus::zvariant::OwnedObjectPath;

use super::intent::Intent;

/// How long an intercepted call may wait for its reply before being dropped.
///
/// Sized for a polkit `auth_admin` prompt the user may leave sitting on screen.
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// Hard cap on simultaneously pending calls, so a reply that never arrives
/// cannot grow the map without bound.
const MAX_PENDING: usize = 256;

/// One intercepted call awaiting its reply.
#[derive(Debug)]
struct Pending {
    /// Object path of the device the call targeted.
    path: OwnedObjectPath,
    /// What the call asked for.
    intent: Intent,
    /// When the call was intercepted, for [`PENDING_TTL`] and for picking the
    /// oldest entry to evict.
    at: Instant,
}

/// Intercepted calls indexed by the serial number their reply will quote in
/// `reply_serial`.
#[derive(Debug, Default)]
pub(super) struct PendingCalls(HashMap<NonZeroU32, Pending>);

impl PendingCalls {
    /// Park an intercepted call until its reply arrives.
    ///
    /// # Parameters
    /// * `serial` - the call message's own serial number, which its reply
    ///   quotes as `reply_serial`.
    /// * `path` - object path of the device the call targeted.
    /// * `intent` - what the call asked for.
    ///
    /// # Post-conditions
    /// `serial` is pending. Every entry older than [`PENDING_TTL`] has been
    /// dropped, and if that still leaves [`MAX_PENDING`] entries or more, the
    /// oldest one is dropped too. An already-pending `serial` is overwritten
    /// (the bus does not reuse a serial while its reply is outstanding, so this
    /// does not happen in practice).
    pub(super) fn record(&mut self, serial: NonZeroU32, path: OwnedObjectPath, intent: Intent) {
        let now = Instant::now();
        self.0
            .retain(|_, pending| now.duration_since(pending.at) < PENDING_TTL);

        while self.0.len() >= MAX_PENDING {
            let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, pending)| pending.at)
                .map(|(serial, _)| *serial)
            else {
                break;
            };

            tracing::warn!(serial = oldest, "dropping the oldest unanswered call");
            self.0.remove(&oldest);
        }

        self.0.insert(
            serial,
            Pending {
                path,
                intent,
                at: now,
            },
        );
    }

    /// Take back the call a reply answers.
    ///
    /// # Parameters
    /// * `reply_serial` - the `reply_serial` header field of a `method_return`
    ///   or `error` message.
    ///
    /// # Returns
    /// `Some((path, intent))` when `reply_serial` matches a pending call, which
    /// stops being pending; `None` when it does not — most replies on the bus
    /// answer calls this listener never intercepted.
    pub(super) fn take(&mut self, reply_serial: NonZeroU32) -> Option<(OwnedObjectPath, Intent)> {
        self.0
            .remove(&reply_serial)
            .map(|pending| (pending.path, pending.intent))
    }
}

#[cfg(test)]
#[path = "pending-tests.rs"]
mod tests;
