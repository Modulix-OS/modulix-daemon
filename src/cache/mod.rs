//! Long-lived caches owned by the daemon (see [`crate::store`]).
//!
//! Unlike the old in-process `gnome-software-plugin/backend`, these survive
//! for as long as `mx-daemon` runs — not just one GNOME Software session —
//! and are shared by every client on the machine.
//!
//! # Shape
//!
//! The one cache type this module exposes is [`FlightCache<K, V>`] (defined
//! in [`flight`]). `K` and `V` are generic over the instantiation, chosen at
//! each call site in `crate::store` — e.g. search results keyed by query
//! string, alternate-of listings and Flathub enrichment keyed by app id,
//! module plugin listings keyed by module id. This module does not itself
//! fix a concrete key or value type.
//!
//! # TTL
//!
//! Configured per instance at construction (`FlightCache::new(ttl, cap)`),
//! not fixed by this module. Current call sites in `crate::store` range
//! from 5 seconds (the "is this already installed" cache, deliberately
//! short-lived) up to 1 hour (Flathub enrichment / license lookups).
//!
//! # Eviction
//!
//! No background thread or timer. Eviction runs synchronously at the end of
//! every `insert`/`get_or_fetch` call: first every already-expired entry
//! (past its TTL) is dropped, then — if the map is still over its `cap` —
//! the single oldest remaining entry is removed. One insertion therefore
//! trims at most one over-cap entry; a burst of inserts converges to `cap`
//! rather than snapping to it instantly.
//!
//! # Memory growth
//!
//! Bounded per instance by the `cap` passed to `FlightCache::new` (current
//! call sites use 512, except a 1-entry cache for a single "already
//! installed" flag). Growth beyond `cap` is only ever transient, between
//! one insertion and the next eviction pass.
//!
//! # Thread-safety
//!
//! `FlightCache` is `Send + Sync` and meant to be shared behind a shared
//! reference (typically reached through a `static` `OnceLock` in
//! `crate::store`): its entry map is guarded by a plain
//! `std::sync::Mutex`, and each entry is a `tokio::sync::OnceCell` so
//! concurrent callers racing on the same missing/expired key share a single
//! in-flight `fetch` instead of each starting their own (the "single-flight"
//! in the name).

/// Single-flight, TTL'd, capacity-bounded cache implementation; see
/// [`FlightCache`] and the module-level docs above for its contract.
pub mod flight;

/// Re-exported so callers write `crate::cache::FlightCache` instead of
/// reaching into the `flight` submodule; see [`flight::FlightCache`] for the
/// full contract (key/value types are generic, fixed per instantiation by
/// the call site).
pub use flight::FlightCache;
