//! Bounded, TTL'd, single-flight cache shared by every read path served by
//! [`crate::store`] (search results, alternate-of listings, module plugins,
//! Flathub enrichment).
//!
//! Without single-flight, two concurrent callers for the same key (e.g. two
//! GNOME Software processes issuing the same query) each spawn their own
//! fetch — a `nix search` subprocess or a Flathub HTTP round-trip.
//! [`FlightCache::get_or_fetch`] instead lets the first caller run `fetch`
//! while every other caller for that key awaits the same result.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;

type Slot<V> = Arc<OnceCell<(V, Instant)>>;

pub struct FlightCache<K, V> {
    entries: Mutex<HashMap<K, Slot<V>>>,
    ttl: Duration,
    cap: usize,
}

impl<K, V> FlightCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
            cap,
        }
    }

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
    pub fn get_fresh(&self, key: &K) -> Option<V> {
        let cell = self.entries.lock().unwrap().get(key)?.clone();
        let (value, at) = cell.get()?;
        (at.elapsed() < self.ttl).then(|| value.clone())
    }

    /// Drops every entry, fresh ones included. For caches whose validity
    /// depends on state this process itself mutates (see
    /// `crate::store::invalidate_installed`), where waiting out the TTL would
    /// serve a value already known to be wrong.
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    /// Directly stores an already-resolved value for `key`.
    pub fn insert(&self, key: K, value: V) {
        let cell = Arc::new(OnceCell::new());
        let _ = cell.set((value, Instant::now()));
        self.entries.lock().unwrap().insert(key, cell);
        self.evict();
    }

    /// Returns the fresh cached value for `key`, or runs `fetch` and caches
    /// its result. Concurrent callers for the same (missing or expired) key
    /// share one `fetch` invocation.
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
                // Stale: hand off to a fresh cell for this key so the next
                // getter (including this one, via `continue`) treats it as a
                // miss. Only swap if nobody else already did — otherwise two
                // racing refreshers would each spawn their own fetch.
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
