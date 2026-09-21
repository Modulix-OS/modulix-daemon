//! Bounded, TTL'd, single-flight cache shared by every read path served by
//! [`crate::store`] (search results, alternate-of listings, module plugins,
//! Flathub enrichment).
//!
//! Without single-flight, two concurrent callers for the same key (e.g. two
//! GNOME Software processes issuing the same query) each spawn their own
//! fetch — a `nix search` subprocess or a Flathub HTTP round-trip.
//! [`FlightCache::get_or_fetch`] instead lets the first caller run `fetch`
//! while every other caller for that key awaits the same result.
//!
//! `FlightCache` has no notion of a failed fetch: `fetch` returns a bare `V`,
//! not a `Result`, so whatever `V` it produces — including a value a caller
//! uses to represent "the underlying call failed" (an empty `Vec`, a `None`)
//! — is cached as a success for the full TTL, with no early retry. See
//! [`FlightCache::get_or_fetch`] for the exact cancellation/panic contract
//! inherited from [`tokio::sync::OnceCell`].

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;

/// One cache slot: a resolved value paired with the [`Instant`] it was
/// resolved at, behind a [`tokio::sync::OnceCell`] so every concurrent
/// resolver for the same key coalesces into a single `fetch`, and behind an
/// [`Arc`] so a caller that already holds a clone of the slot keeps seeing
/// it resolve even after [`FlightCache::clear`] or a stale-entry swap in
/// [`FlightCache::get_or_fetch`] has removed it from the backing map.
///
/// # Type parameters
/// - `V`: the cached value type.
type Slot<V> = Arc<OnceCell<(V, Instant)>>;

/// A per-key, TTL'd cache that coalesces concurrent misses for the same key
/// into a single `fetch` call. See the module docs for the rationale and
/// [`Self::get_or_fetch`] for the exact single-flight contract.
///
/// # Type parameters
/// - `K`: the key type. Bounded `Eq + Hash + Clone` on the impl block —
///   cloned on every lookup to index and, on a miss, to insert into the
///   backing map.
/// - `V`: the cached value type. Bounded `Clone` on the impl block — every
///   reader gets an owned clone of the cached value rather than a
///   reference, so [`Self::get_or_fetch`] and [`Self::get_fresh`] can
///   return `V` directly while the cache keeps serving its own copy.
pub struct FlightCache<K, V> {
    /// The backing map, one [`Slot`] per key. Guarded by a synchronous
    /// [`Mutex`] that is held only for the map lookup/insert/removal itself
    /// — never across a `fetch` `.await`, which happens after the cloned
    /// [`Slot`] handle has been released back out of the lock.
    entries: Mutex<HashMap<K, Slot<V>>>,
    /// How long a resolved entry is served before a lookup treats it as a
    /// miss and re-runs `fetch`.
    ttl: Duration,
    /// Soft upper bound on the number of entries kept. Enforced by
    /// [`Self::evict`], which removes at most one over-capacity entry per
    /// call — see its doc for why a burst of concurrent insertions can
    /// leave the map above `cap` for a while.
    cap: usize,
}

impl<K, V> FlightCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    /// Builds an empty cache.
    ///
    /// # Parameters
    /// - `ttl`: how long a resolved entry stays fresh; see the field doc on
    ///   [`Self::ttl`].
    /// - `cap`: soft capacity passed through to [`Self::evict`]; see the
    ///   field doc on [`Self::cap`].
    ///
    /// # Returns
    /// A `FlightCache` with no entries.
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            cap,
        }
    }

    /// Returns the [`Slot`] for `key`, inserting a fresh, unresolved one
    /// first if `key` is not yet present.
    ///
    /// # Parameters
    /// - `key`: the key to look up. Cloned into the map only when a new
    ///   entry has to be inserted.
    ///
    /// # Pre-conditions
    /// None.
    ///
    /// # Post-conditions
    /// The map contains an entry for `key` — pre-existing, or a freshly
    /// inserted empty [`Slot`]. [`Self::entries`]'s lock is held only for
    /// this lookup/insert and is released before the returned `Slot` is
    /// awaited on by the caller.
    ///
    /// # Returns
    /// A clone of the slot's `Arc`. Because the map lookup and insertion
    /// are serialized by the mutex, every concurrent caller for the same
    /// `key` is handed a clone of the *same* `Arc<OnceCell<..>>` — this is
    /// what lets [`Self::get_or_fetch`] coalesce concurrent misses onto one
    /// [`tokio::sync::OnceCell`].
    fn cell_for(&self, key: &K) -> Slot<V> {
        self.entries
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    }

    /// Drops every expired entry, then — if still over `cap` — the single
    /// oldest remaining one. Cheap at `cap`'s scale (a few hundred entries),
    /// so it runs on every insertion rather than on a timer.
    ///
    /// # Pre-conditions
    /// None.
    ///
    /// # Post-conditions
    /// Every entry whose slot is resolved (`cell.get()` is `Some`) and
    /// whose age exceeds `ttl` is removed. An entry whose slot is not yet
    /// resolved (in flight, or a freshly inserted empty slot from
    /// [`Self::cell_for`]) is kept regardless of age, and is never a
    /// candidate for capacity eviction either. If the map is still over
    /// `cap` after the TTL sweep, exactly one further entry — the oldest
    /// remaining resolved one — is removed. Consequently, a burst of
    /// concurrent insertions can leave the map above `cap` until a later
    /// call to this method brings it down by one more.
    fn evict(&self) {
        let mut guard = self.entries.lock().unwrap();
        let ttl = self.ttl;
        guard.retain(|_, cell| cell.get().is_none_or(|(_, at)| at.elapsed() < ttl));
        if guard.len() > self.cap
            && let Some(oldest) = guard
                .iter()
                .filter_map(|(k, cell)| cell.get().map(|(_, at)| (k.clone(), *at)))
                .min_by_key(|(_, at)| *at)
                .map(|(k, _)| k)
        {
            guard.remove(&oldest);
        }
    }

    /// A cached, still-fresh value for `key`, if any — never runs `fetch`.
    /// Used by batch paths that populate the cache themselves via
    /// [`Self::insert`] (their own concurrent fetch is already single-flight
    /// by construction: one batched call covers every key in it).
    ///
    /// # Parameters
    /// - `key`: the key to look up. Unlike [`Self::cell_for`], a miss does
    ///   not insert anything into the map — this call never registers `key`
    ///   for single-flight coalescing.
    ///
    /// # Pre-conditions
    /// None.
    ///
    /// # Returns
    /// `Some(value)` if `key` has a resolved entry younger than `ttl`;
    /// `None` if `key` is absent from the map, its slot is still
    /// unresolved (in flight), or its resolved value is older than `ttl`.
    pub fn get_fresh(&self, key: &K) -> Option<V> {
        let cell = self.entries.lock().unwrap().get(key)?.clone();
        let (value, at) = cell.get()?;
        (at.elapsed() < self.ttl).then(|| value.clone())
    }

    /// Drops every entry, fresh ones included. For caches whose validity
    /// depends on state this process itself mutates (see
    /// `crate::store::invalidate_installed`), where waiting out the TTL would
    /// serve a value already known to be wrong.
    ///
    /// # Pre-conditions
    /// None.
    ///
    /// # Post-conditions
    /// The map is empty. This does **not** cancel a `fetch` that is
    /// currently in flight for a cleared key: a [`Slot`] is an `Arc`, so
    /// the task driving [`tokio::sync::OnceCell::get_or_init`] for it keeps
    /// running to completion independently of the map entry being removed.
    /// A caller that obtained its `Slot` clone before this call still
    /// observes that fetch resolve; any caller that looks `key` up
    /// afterward goes through [`Self::cell_for`], is handed a brand-new
    /// empty slot, and triggers its own `fetch` — the in-flight fetch's
    /// eventual result is never written back into the cache.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    /// Directly stores an already-resolved value for `key`, bypassing
    /// `fetch` entirely.
    ///
    /// # Parameters
    /// - `key`: the key to store under. Any existing slot for `key` is
    ///   replaced; if that slot was still in flight, the fetch driving it
    ///   keeps running (as in [`Self::clear`]) but its result is no longer
    ///   reachable through the cache once resolved.
    /// - `value`: the value to store, timestamped with [`Instant::now`] at
    ///   the moment of insertion.
    ///
    /// # Pre-conditions
    /// None.
    ///
    /// # Post-conditions
    /// `key` maps to a resolved slot holding `value`, immediately fresh.
    /// [`Self::evict`] then runs, so this call may also drop other expired
    /// or over-capacity entries as a side effect.
    pub fn insert(&self, key: K, value: V) {
        let cell = Arc::new(OnceCell::new());
        let _ = cell.set((value, Instant::now()));
        self.entries.lock().unwrap().insert(key, cell);
        self.evict();
    }

    /// Returns the fresh cached value for `key`, or runs `fetch` and caches
    /// its result. Concurrent callers for the same (missing or expired) key
    /// share one `fetch` invocation.
    ///
    /// # Parameters
    /// - `key`: the key to look up. Cloned via [`Self::cell_for`], and
    ///   again if a stale entry has to be swapped out for a fresh slot.
    /// - `fetch`: produces the value to cache on a miss. At most one
    ///   `fetch` invocation actually resolves a given slot — every other
    ///   concurrent caller for the same key awaits that resolution and
    ///   receives a clone of its result instead of calling its own `fetch`.
    ///   Because `fetch` is `FnOnce`, each call to `get_or_fetch` supplies
    ///   its own closure instance; per
    ///   [`tokio::sync::OnceCell::get_or_init`]'s contract, if the caller
    ///   currently driving the resolution is cancelled (its future is
    ///   dropped) or its `fetch` panics, that attempt is abandoned and the
    ///   slot stays unresolved — if another caller is still waiting on the
    ///   same slot, *that* caller's own `fetch` closure becomes the new
    ///   resolution attempt (the original invocation is not resumed or
    ///   retried on its behalf); if no other caller is waiting, the slot is
    ///   simply left unresolved and the next `get_or_fetch` for `key`
    ///   starts a brand-new `fetch`.
    ///
    /// # Pre-conditions
    /// None.
    ///
    /// # Post-conditions
    /// On successful return, `key` mapped to a resolved, fresh slot holding
    /// the returned value at the moment it resolved — unless a concurrent
    /// [`Self::clear`] or [`Self::insert`] raced it out of the map
    /// afterward (see their docs). When the slot found for `key` holds a
    /// stale (TTL-expired) value, it is swapped out for a fresh, unresolved
    /// slot before the miss is retried — but only by whichever concurrent
    /// caller observes the map still pointing at that exact stale slot first
    /// (checked via `Arc::ptr_eq`); every other caller racing on the same
    /// stale entry simply loops and picks up the slot the winner installed.
    /// This keeps a burst of callers noticing the same expired entry down to
    /// exactly one fresh slot — and, downstream, at most one `fetch` — rather
    /// than one per caller.
    ///
    /// # Errors
    /// `fetch` returns a bare `V`, not a `Result` — this cache has no
    /// concept of a failed fetch. Whatever `V` `fetch` produces, including
    /// a value a caller uses to represent failure (an empty collection, a
    /// `None`), is cached as-is for the full `ttl`; it is not distinguished
    /// from a successful fetch and is not retried early.
    ///
    /// # Panics
    /// Does not panic itself, but if `fetch` panics while its caller is the
    /// one resolving the slot, that panic propagates out of this call (via
    /// [`tokio::sync::OnceCell::get_or_init`]) and the slot is left
    /// unresolved — see the `fetch` parameter doc above.
    ///
    /// # Returns
    /// The fresh value for `key`: an existing cached value younger than
    /// `ttl` if one exists, otherwise the result of the (possibly shared)
    /// `fetch` call.
    pub async fn get_or_fetch<F, Fut>(&self, key: K, fetch: F) -> V
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V>,
    {
        loop {
            let cell = self.cell_for(&key);
            if let Some((value, fetched_at)) = cell.get() {
                if fetched_at.elapsed() < self.ttl {
                    return value.clone();
                }
                let mut guard = self.entries.lock().unwrap();
                if guard.get(&key).is_some_and(|slot| Arc::ptr_eq(slot, &cell)) {
                    guard.insert(key.clone(), Arc::new(OnceCell::new()));
                }
                continue;
            }
            let (value, _) = cell
                .get_or_init(|| async { (fetch().await, Instant::now()) })
                .await
                .clone();
            self.evict();
            return value;
        }
    }
}
