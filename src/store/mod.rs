//! `org.modulix.Store1`: the daemon's read-only interface — search, listings,
//! metadata enrichment, alternate sources. Replaces the in-process
//! `gnome-software-plugin/backend` FFI layer; every store (GNOME Software
//! today, others tomorrow) becomes a thin D-Bus client of this interface.
//!
//! Payloads are typed `a{sv}` (see [`entry`]), not JSON: the daemon stays
//! neutral (no GNOME-Software-specific number crosses the bus) and every
//! client gets structural typing for free.

pub mod entry;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use modulix_core_utils::module_info::ModuleInfo;
use modulix_core_utils::package_info::{self, NixPackage};
use modulix_core_utils::{
    AppInfoGui, AppInfoMinimal, FlatpakInfo, install_module, install_package, package_index,
};

use crate::cache::FlightCache;
use entry::{
    AppEntry, Dict, EnrichEntry, InputEntry, PluginEntry, alt_sort_key, base_attr,
    collect_screenshots, dedup_by_group, icon_base_name, icon_name_for_app_id, module_entry,
    module_rows_for_app_id, package_entry, variant_label, variant_rank,
};

/// Cap shared by every cache in this module: one entry per distinct query/
/// app-id ever seen would otherwise grow unbounded over the daemon's
/// lifetime — see [`crate::cache::FlightCache`].
const CACHE_CAP: usize = 512;

/// Cap on the number of concurrent fetches within one batched call: the
/// `for_each_concurrent`/`buffer_unordered` fan-out in [`resolve_all`]
/// (module resolution over the network), [`Store::get_app_enrichment`]
/// (Flathub HTTP round-trips) and [`Store::get_package_licenses`] (`nix
/// eval` invocations) is all bounded to this many in-flight requests.
const CONCURRENCY_LIMIT: usize = 8;

/// TTL of [`SEARCH_CACHE`]: how long a `search_packages`/`search_modules`
/// result page is served from cache before the next call for the same
/// `(kind, query, max)` re-runs the search. Short — search hits are cheap to
/// recompute (index lookup or, at worst, a `nix search` subprocess) and
/// staleness here is only cosmetic.
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(60);

/// TTL of [`ALT_CACHE`]: how long a `packages_for_app_id` alternate-sources
/// listing is served from cache. Longer than [`SEARCH_CACHE_TTL`] — the set
/// of nix variants and Modulix modules providing a given app-id changes far
/// less often than search relevance does.
const ALT_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// TTL of [`ENRICH_CACHE`], and — via [`license_cache`] — of
/// [`LICENSE_CACHE`] too: how long Flathub enrichment / a resolved SPDX
/// license is cached. Generous — both change only when nixpkgs or the app's
/// Flathub listing does, and a `nix eval` per license is not cheap to redo.
const ENRICH_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// TTL of [`PLUGINS_CACHE`]: how long a module's plugin listing
/// (`list_module_plugins`) is served from cache before being re-evaluated
/// against the live nixpkgs plugin namespace (one `nix eval` per module's
/// worth of plugin attributes, via [`ModuleInfo::list_plugins`]).
const PLUGINS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// Short on purpose: the configuration can also change behind the daemon's
/// back (the `mx` CLI, a hand-edited `package.nix`). Writes that go *through*
/// the daemon don't wait for it — they call [`invalidate_installed`].
const INSTALLED_CACHE_TTL: Duration = Duration::from_secs(5);

/// TTL of [`UPDATE_CACHE`]: how long an outdated-inputs listing is served
/// from cache before [`Store::list_outdated_inputs`] re-probes every flake
/// input's upstream revision. An hour, since each probe is a `nix flake
/// metadata` network round-trip per input — a caller wanting a fresher
/// answer passes `force_refresh: true` rather than waiting this out (see
/// `Store::list_outdated_inputs`). A completed `UpdateSystem` call also
/// clears this early via [`invalidate_updates`].
const UPDATE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// Ceiling on `nix eval` invocations a single `GetPackageLicenses` call may
/// trigger. GNOME Software asks for a license on the details page (one app),
/// so this only bounds a pathological caller passing a whole search page.
const MAX_LICENSE_EVALS: usize = 16;

/// Timeout budget for the best-effort Modulix-module lookup prepended to the
/// alternate-of list: this runs on every details-page open, so a
/// slow/unreachable module index must not stall the whole popover.
const MODULE_LOOKUP_TIMEOUT: Duration = Duration::from_millis(500);

/// Max nix variants pulled from a live pname search when populating the
/// alternate-of dropdown of a non-AppStream group (nvtopPackages.*).
const MAX_VARIANT_SEARCH: u32 = 50;

/// Cache key for [`SEARCH_CACHE`]: `(kind, query, max)`, where `kind` is
/// `"pkg"` for [`Store::search_packages`] or `"mod"` for
/// [`Store::search_modules`] — the two searches never collide on the same
/// key even for an identical `query`/`max` pair.
type SearchKey = (&'static str, String, u32);

/// Single-flight, TTL'd cache of `search_packages`/`search_modules` result
/// pages, keyed by [`SearchKey`]. Populated *before* [`AppEntry::installed`]
/// is stamped and before [`dedup_by_group`] runs, so a cached page always
/// reflects the raw, un-deduplicated search hits — see [`search_cache`].
static SEARCH_CACHE: OnceLock<FlightCache<SearchKey, Vec<AppEntry>>> = OnceLock::new();

/// Single-flight, TTL'd cache of `packages_for_app_id` alternate-sources
/// listings, keyed by the group id (see [`Store::packages_for_app_id`]) —
/// see [`alt_cache`].
static ALT_CACHE: OnceLock<FlightCache<String, Vec<AppEntry>>> = OnceLock::new();

/// Single-flight, TTL'd cache of Flathub enrichment, keyed by AppStream app
/// id. `None` is cached, not just `Some`, so an app-id known to have no
/// Flathub listing does not retry the HTTP request on every call — see
/// [`enrich_cache`] and [`fetch_enrichment`].
static ENRICH_CACHE: OnceLock<FlightCache<String, Option<EnrichEntry>>> = OnceLock::new();

/// Single-flight, TTL'd cache of `list_module_plugins` results, keyed by
/// module name — see [`plugins_cache`].
static PLUGINS_CACHE: OnceLock<FlightCache<String, Vec<PluginEntry>>> = OnceLock::new();

/// Single-flight, TTL'd cache of resolved SPDX licenses, keyed by nixpkgs
/// attribute. `None` is cached the same way as [`ENRICH_CACHE`]'s, so an
/// attribute with no resolvable license does not re-run `nix eval` on every
/// call — see [`license_cache`].
static LICENSE_CACHE: OnceLock<FlightCache<String, Option<String>>> = OnceLock::new();

/// Single-flight, TTL'd cache of [`InstalledSets`], with a single key (`()`
/// — there is only ever one system configuration to describe) — see
/// [`installed_cache`].
static INSTALLED_CACHE: OnceLock<FlightCache<(), Arc<InstalledSets>>> = OnceLock::new();
/// Single-flight, TTL'd cache of the outdated-inputs listing, with a single
/// key (`()` — there is only ever one `flake.lock` to describe) — see
/// [`update_cache`].
static UPDATE_CACHE: OnceLock<FlightCache<(), Arc<Vec<InputEntry>>>> = OnceLock::new();
/// Key = module name, value = the **bare** plugin names (last component of
/// each `pkgs.<namespace>.<plugin>` token) currently listed under
/// `mx.<module>.plugins`. Matching by bare name rather than the full token
/// avoids re-resolving `ModuleInfo` just to learn the namespace outside the
/// `plugins_cache()` fetch closure — a manually-added entry from a different
/// namespace than the module's own would false-positive here, an accepted
/// trade-off.
static INSTALLED_PLUGINS_CACHE: OnceLock<FlightCache<String, Arc<HashSet<String>>>> =
    OnceLock::new();
/// `(module, bare_plugin_name)` pair, the element of [`INSTALLED_PLUGINS_ALL_CACHE`].
type InstalledPluginPairs = Arc<Vec<(String, String)>>;
/// Single-flight, TTL'd cache of every [`InstalledPluginPairs`] pair
/// currently installed, across all enabled modules — same single-key
/// rationale as [`INSTALLED_CACHE`]. Backs `Store1.ListInstalledPlugins`'s
/// `module == ""` case.
static INSTALLED_PLUGINS_ALL_CACHE: OnceLock<FlightCache<(), InstalledPluginPairs>> =
    OnceLock::new();

/// The process-wide [`SEARCH_CACHE`], created with [`SEARCH_CACHE_TTL`] and
/// [`CACHE_CAP`] on first access.
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn search_cache() -> &'static FlightCache<SearchKey, Vec<AppEntry>> {
    SEARCH_CACHE.get_or_init(|| FlightCache::new(SEARCH_CACHE_TTL, CACHE_CAP))
}

/// The process-wide [`ALT_CACHE`], created with [`ALT_CACHE_TTL`] and
/// [`CACHE_CAP`] on first access.
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn alt_cache() -> &'static FlightCache<String, Vec<AppEntry>> {
    ALT_CACHE.get_or_init(|| FlightCache::new(ALT_CACHE_TTL, CACHE_CAP))
}

/// The process-wide [`ENRICH_CACHE`], created with [`ENRICH_CACHE_TTL`] and
/// [`CACHE_CAP`] on first access.
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn enrich_cache() -> &'static FlightCache<String, Option<EnrichEntry>> {
    ENRICH_CACHE.get_or_init(|| FlightCache::new(ENRICH_CACHE_TTL, CACHE_CAP))
}

/// The process-wide [`PLUGINS_CACHE`], created with [`PLUGINS_CACHE_TTL`]
/// and [`CACHE_CAP`] on first access.
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn plugins_cache() -> &'static FlightCache<String, Vec<PluginEntry>> {
    PLUGINS_CACHE.get_or_init(|| FlightCache::new(PLUGINS_CACHE_TTL, CACHE_CAP))
}

/// Same short TTL as [`installed_cache`], for the same reason: the
/// configuration can change behind the daemon's back, and a write that goes
/// through the daemon calls [`invalidate_installed`] rather than waiting it
/// out.
fn installed_plugins_cache() -> &'static FlightCache<String, Arc<HashSet<String>>> {
    INSTALLED_PLUGINS_CACHE.get_or_init(|| FlightCache::new(INSTALLED_CACHE_TTL, CACHE_CAP))
}

/// Same single-key, cap-1 pattern as [`installed_cache`]: there is only ever
/// one system configuration, so the cache needs exactly one slot.
fn installed_plugins_all_cache() -> &'static FlightCache<(), InstalledPluginPairs> {
    INSTALLED_PLUGINS_ALL_CACHE.get_or_init(|| FlightCache::new(INSTALLED_CACHE_TTL, 1))
}

/// The process-wide [`LICENSE_CACHE`], created with [`CACHE_CAP`] on first
/// access. Licenses change only when nixpkgs does, so this reuses
/// [`ENRICH_CACHE_TTL`] rather than defining its own constant.
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn license_cache() -> &'static FlightCache<String, Option<String>> {
    LICENSE_CACHE.get_or_init(|| FlightCache::new(ENRICH_CACHE_TTL, CACHE_CAP))
}

/// What the system configuration currently declares: nixpkgs attributes in
/// `environment.systemPackages`, and enabled `mx.*` modules.
///
/// Both listings are pure parsing of the config files — no `nix eval`, no
/// network — which is what makes it affordable to consult them on the search
/// path (see [`installed_sets`]).
///
/// # Fields
/// * `packages` - nixpkgs attribute names listed in
///   `environment.systemPackages`.
/// * `modules` - keys of the `mx.*` modules currently enabled.
#[derive(Default)]
struct InstalledSets {
    packages: HashSet<String>,
    modules: HashSet<String>,
}

impl InstalledSets {
    /// Whether `entry` is currently in the system configuration.
    ///
    /// # Parameters
    /// * `entry` - the entry to check; its `kind` selects which of
    ///   `packages`/`modules` is consulted and its `name` is looked up in
    ///   that set.
    ///
    /// # Returns
    /// `true` when `entry.name` is a member of the set matching
    /// `entry.kind` (`"module"` → `modules`, anything else → `packages`) —
    /// a module and a package sharing a name are tracked independently.
    fn contains(&self, entry: &AppEntry) -> bool {
        if entry.kind == "module" {
            self.modules.contains(&entry.name)
        } else {
            self.packages.contains(&entry.name)
        }
    }
}

/// The process-wide [`INSTALLED_CACHE`], created with
/// [`INSTALLED_CACHE_TTL`] on first access. Uses a cap of 1: the cache has a
/// single key (`()`), so any non-zero capacity suffices.
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn installed_cache() -> &'static FlightCache<(), Arc<InstalledSets>> {
    INSTALLED_CACHE.get_or_init(|| FlightCache::new(INSTALLED_CACHE_TTL, 1))
}

/// The process-wide [`UPDATE_CACHE`], created with [`UPDATE_CACHE_TTL`] on
/// first access. Uses a cap of 1, same single-key rationale as
/// [`installed_cache`].
///
/// # Returns
/// A `'static` reference to the cache, shared by every caller for the life
/// of the daemon.
fn update_cache() -> &'static FlightCache<(), Arc<Vec<InputEntry>>> {
    UPDATE_CACHE.get_or_init(|| FlightCache::new(UPDATE_CACHE_TTL, 1))
}

/// The cached installed sets. Behind an `Arc`: one search stamps hundreds of
/// entries against the same snapshot, and cloning the sets each time would
/// dwarf the lookup itself.
///
/// # Returns
/// The current [`InstalledSets`], served from [`installed_cache`] when
/// fresh; on a cache miss, re-parses `environment.systemPackages` and the
/// enabled `mx.*` modules from the on-disk configuration (pure file
/// parsing — no `nix eval`, no network, per [`InstalledSets`]'s own doc).
/// Either sub-listing that fails to parse is logged at `warn` and treated
/// as empty, never propagated: a partial or unreadable configuration must
/// still let the rest of the store answer.
async fn installed_sets() -> Arc<InstalledSets> {
    installed_cache()
        .get_or_fetch((), || async {
            let dir = crate::config_dir::config_dir();
            let packages = tokio::task::spawn_blocking(move || {
                install_package::list_installed_package_names(dir)
            })
            .await
            .map_err(|e| tracing::warn!(error = %e, "installed_sets: packages: join"))
            .ok()
            .and_then(|r| {
                r.map_err(|e| tracing::warn!(error = %e, "installed_sets: packages"))
                    .ok()
            })
            .unwrap_or_default();
            let modules =
                tokio::task::spawn_blocking(move || install_module::list_enabled_module_names(dir))
                    .await
                    .map_err(|e| tracing::warn!(error = %e, "installed_sets: modules: join"))
                    .ok()
                    .and_then(|r| {
                        r.map_err(|e| tracing::warn!(error = %e, "installed_sets: modules"))
                            .ok()
                    })
                    .unwrap_or_default();
            Arc::new(InstalledSets {
                packages: packages.into_iter().collect(),
                modules: modules.into_iter().collect(),
            })
        })
        .await
}

/// Fills in [`AppEntry::installed`] for a batch of entries.
///
/// Always applied **after** a [`FlightCache`] read, never inside the fetch
/// closure: `SEARCH_CACHE` keeps a result for 60s and `ALT_CACHE` for 5
/// minutes — longer than an install takes — so a flag baked into the cached
/// value would leave the store offering "Install" for an app the user has
/// just installed. Must also run *before* [`dedup_by_group`], which uses the
/// flag to elect each group's representative.
///
/// # Parameters
/// * `entries` - the batch to stamp, mutated in place.
///
/// # Pre-conditions
/// None on `entries`' `installed` field — every entry's flag is overwritten
/// unconditionally.
///
/// # Post-conditions
/// Every entry's `installed` reflects [`installed_sets`]'s current snapshot
/// (itself cached for [`INSTALLED_CACHE_TTL`], 5s).
async fn stamp_installed(entries: &mut [AppEntry]) {
    let sets = installed_sets().await;
    for entry in entries.iter_mut() {
        entry.installed = sets.contains(entry);
    }
}

/// The bare names of the plugins currently installed for `module` (see
/// [`INSTALLED_PLUGINS_CACHE`]).
///
/// # Parameters
/// * `module` - module key whose plugin list (`mx.<module>.plugins`) is
///   consulted.
///
/// # Returns
/// The bare plugin names (last dotted component of each
/// `pkgs.<namespace>.<plugin>` token) currently listed for `module`, served
/// from [`installed_plugins_cache`] when fresh; on a miss, re-parsed from
/// the on-disk configuration. A parse failure is logged at `warn` and
/// treated as an empty set rather than propagated.
async fn installed_plugins(module: &str) -> Arc<HashSet<String>> {
    installed_plugins_cache()
        .get_or_fetch(module.to_string(), || async {
            let dir = crate::config_dir::config_dir();
            let module_owned = module.to_string();
            let attrs = tokio::task::spawn_blocking(move || {
                install_module::list_installed_plugin_attrs(dir, &module_owned)
            })
            .await
            .map_err(|e| tracing::warn!(error = %e, module = %module, "installed_plugins: join"))
            .ok()
            .and_then(|r| {
                r.map_err(|e| tracing::warn!(error = %e, module = %module, "installed_plugins"))
                    .ok()
            })
            .unwrap_or_default();
            Arc::new(
                attrs
                    .into_iter()
                    .map(|attr| attr.rsplit('.').next().unwrap_or(&attr).to_string())
                    .collect(),
            )
        })
        .await
}

/// Every `(module, bare_plugin_name)` pair currently installed, across all
/// enabled modules (see [`INSTALLED_PLUGINS_ALL_CACHE`]).
///
/// Backs `Store1.ListInstalledPlugins`, whether it asks for one module or
/// every module — a single fetch here is filtered per call rather than
/// re-reading the configuration per module, since
/// `modulix_core_utils::install_module::list_installed_plugins` already
/// walks every enabled module in one `module.nix` read.
///
/// # Returns
/// The pairs from a fresh [`installed_plugins_all_cache`] read, or from the
/// on-disk configuration on a cache miss. A parse failure is logged at
/// `warn` and treated as empty rather than propagated.
async fn installed_plugins_all() -> InstalledPluginPairs {
    installed_plugins_all_cache()
        .get_or_fetch((), || async {
            let dir = crate::config_dir::config_dir();
            let plugins =
                tokio::task::spawn_blocking(move || install_module::list_installed_plugins(dir))
                    .await
                    .map_err(|e| tracing::warn!(error = %e, "installed_plugins_all: join"))
                    .ok()
                    .and_then(|r| {
                        r.map_err(|e| tracing::warn!(error = %e, "installed_plugins_all"))
                            .ok()
                    })
                    .unwrap_or_default();
            Arc::new(plugins)
        })
        .await
}

/// Fills in [`PluginEntry::installed`] for a module's plugin listing. Same
/// ordering rule as [`stamp_installed`]: after the [`plugins_cache`] read,
/// never inside its fetch closure — that cache lives
/// [`PLUGINS_CACHE_TTL`] (5 minutes), far longer than an install takes.
///
/// # Parameters
/// * `module` - module key the plugin `entries` belong to.
/// * `entries` - the plugin listing to stamp, mutated in place.
///
/// # Post-conditions
/// Every entry's `installed` reflects [`installed_plugins`]'s current
/// snapshot for `module`.
async fn stamp_plugins_installed(module: &str, entries: &mut [PluginEntry]) {
    let installed = installed_plugins(module).await;
    for entry in entries.iter_mut() {
        entry.installed = installed.contains(&entry.name);
    }
}

/// Drops the cached installed sets, so the next read re-parses the
/// configuration. Called by [`crate::daemon`] after a successful install or
/// uninstall: without it the store would keep showing the pre-install state
/// for up to [`INSTALLED_CACHE_TTL`].
///
/// # Post-conditions
/// [`INSTALLED_CACHE`], [`INSTALLED_PLUGINS_CACHE`] and
/// [`INSTALLED_PLUGINS_ALL_CACHE`] are all cleared —
/// fresh entries included, not just expired ones — so the very next read on
/// either path re-parses the on-disk configuration instead of serving a
/// value already known to be stale.
pub fn invalidate_installed() {
    installed_cache().clear();
    installed_plugins_cache().clear();
    installed_plugins_all_cache().clear();
}

/// Drops the cached outdated-inputs listing, so the next
/// `ListOutdatedInputs` re-probes every flake input's upstream revision.
/// Called by [`crate::daemon`] after a successful `UpdateSystem`: without it
/// the store would keep reporting the pre-update set of outdated inputs for
/// up to [`UPDATE_CACHE_TTL`].
///
/// # Post-conditions
/// [`UPDATE_CACHE`] is cleared unconditionally — a fresh entry included, not
/// just an expired one.
pub fn invalidate_updates() {
    update_cache().clear();
}

/// Resolve every module concurrently (bounded) instead of one at a time —
/// `resolve()` does a network round-trip per module.
///
/// # Parameters
/// * `modules` - the modules to resolve; each gets its Flathub or
///   `metadata.json` payload fetched (see [`ModuleInfo::resolve`]).
///
/// # Post-conditions
/// Every module in `modules` has been resolved, with at most
/// [`CONCURRENCY_LIMIT`] fetches in flight at once. A module whose fetch
/// fails is left with an empty resolved payload (swallowed inside
/// `resolve()`/`get_flatpak`/`get_metadata`) — this function itself never
/// fails.
async fn resolve_all(modules: &[ModuleInfo]) {
    stream::iter(modules)
        .for_each_concurrent(CONCURRENCY_LIMIT, |module| module.resolve())
        .await;
}

/// Converts a library `Result` into a `zbus::fdo::Result`, logging the
/// error before it is turned into a D-Bus fault.
///
/// # Parameters
/// * `r` - the result to convert.
/// * `ctx` - short description of the operation, used both as the
///   `tracing::warn!` message and as the prefix of the returned error's
///   message.
///
/// # Returns
/// `Ok(value)` unchanged on success.
///
/// # Errors
/// [`zbus::fdo::Error::Failed`], carrying `"{ctx}: {e}"`, whenever `r` is
/// `Err`; the original error is also logged at `warn` via `ctx` before being
/// discarded.
fn to_result<T>(r: Result<T, impl std::fmt::Display>, ctx: &str) -> zbus::fdo::Result<T> {
    r.map_err(|e| {
        tracing::warn!(error = %e, "{ctx}");
        zbus::fdo::Error::Failed(format!("{ctx}: {e}"))
    })
}

/// The `org.modulix.Store1` interface implementation: the daemon's
/// unprivileged, read-only D-Bus surface (see the module docs above). Holds
/// no state of its own — every cache it reads through is a module-level
/// `static` (see [`SEARCH_CACHE`], [`ALT_CACHE`], [`ENRICH_CACHE`],
/// [`PLUGINS_CACHE`], [`LICENSE_CACHE`], [`INSTALLED_CACHE`]), shared by
/// every `Store` instance and every connected client.
#[derive(Default)]
pub struct Store;

#[zbus::interface(name = "org.modulix.Store1")]
impl Store {
    /// D-Bus property `IndexReady` (`b`): whether the on-disk nix package
    /// index is currently servable.
    ///
    /// # Returns
    /// `true` when [`Store::search_packages`] and [`build_alt_entries`]'s
    /// pname-search branch would be served from the mmap'd index; `false`
    /// when they would fall back to a live `nix search` subprocess instead.
    /// Search still works either way — `false` just means slower answers on
    /// a cold cache.
    #[zbus(property)]
    async fn index_ready(&self) -> bool {
        package_index::is_ready().await
    }

    /// Serves `SearchPackages(s query, u max) -> aa{sv}`: free-text search
    /// over the nixpkgs package set.
    ///
    /// Served from [`SEARCH_CACHE`] when a fresh entry exists for `("pkg",
    /// query, max)` ([`SEARCH_CACHE_TTL`], 60s); concurrent callers for the
    /// same key share one in-flight search rather than each spawning their
    /// own (see `FlightCache::get_or_fetch`). On a cache miss the search
    /// itself is served out of the on-disk nix package index in the low
    /// milliseconds when [`index_ready`](Store::index_ready) is `true`,
    /// otherwise it falls back to a live `nix search` subprocess (roughly
    /// 1.3-1.7s warm, ~20s cold). Caching happens *before* dedup: which
    /// variant represents a group depends on what is installed, and the
    /// cached value must not freeze that in. `AppEntry::installed` is
    /// always stamped fresh, cache hit or not (see [`stamp_installed`]), so
    /// a cached page never carries a stale install flag; same-app-id/pname
    /// variants are then collapsed by [`dedup_by_group`] — relevance order
    /// is still what it falls back on when no variant of a group is
    /// installed — the dropped ones stay reachable through
    /// [`Store::packages_for_app_id`].
    ///
    /// # Parameters
    /// * `query` - free-text search terms.
    /// * `max` - upper bound on the number of rows returned, applied before
    ///   dedup — the final reply may be shorter.
    ///
    /// # Returns
    /// One row per matching, deduplicated package (see `entry::AppEntry`),
    /// best match first. A query matching nothing yields an empty array,
    /// never an error.
    ///
    /// # Errors
    /// Never returns `Err`: a search failure (I/O, `nix` invocation, or
    /// JSON parsing, from `NixPackage::search_scored`) is logged at `warn`
    /// and yields an empty result for that query instead.
    async fn search_packages(&self, query: &str, max: u32) -> zbus::fdo::Result<Vec<Dict>> {
        let key = ("pkg", query.to_string(), max);
        let mut entries = search_cache()
            .get_or_fetch(key, || async move {
                match NixPackage::search_scored(query, max).await {
                    Ok(pkgs) => pkgs
                        .iter()
                        .map(|(score, pkg)| package_entry(pkg, *score))
                        .collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, query, "search_packages");
                        Vec::new()
                    }
                }
            })
            .await;
        stamp_installed(&mut entries).await;
        let entries = dedup_by_group(entries);
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    /// Serves `SearchModules(s query, u max) -> aa{sv}`: free-text search
    /// over the Modulix module catalogue.
    ///
    /// Cached the same way as [`Store::search_packages`], under the
    /// `("mod", query, max)` key of the same [`SEARCH_CACHE`] (so a package
    /// and a module search never collide even for an identical
    /// `query`/`max`). On a cache miss, matching modules are resolved
    /// concurrently — up to [`CONCURRENCY_LIMIT`] Flathub/`metadata.json`
    /// fetches in flight at once, one network round-trip per module —
    /// *inside* the cached fetch closure, so the resolved icon/description
    /// are themselves cached for [`SEARCH_CACHE_TTL`] rather than re-fetched
    /// on every cache hit. `AppEntry::installed` is stamped fresh after
    /// every cache read, exactly as in `search_packages`.
    ///
    /// # Parameters
    /// * `query` - free-text search terms.
    /// * `max` - upper bound on the number of rows returned.
    ///
    /// # Returns
    /// One row per matching module (see `entry::AppEntry`), best match
    /// first. Unlike `search_packages`, results are not deduplicated by
    /// group — modules only ever group by their own app-id, if any. A query
    /// matching nothing yields an empty array, never an error.
    ///
    /// # Errors
    /// Never returns `Err`: a failure to fetch or parse the module index is
    /// logged at `warn` and yields an empty result for that query instead.
    async fn search_modules(&self, query: &str, max: u32) -> zbus::fdo::Result<Vec<Dict>> {
        let key = ("mod", query.to_string(), max);
        let mut entries = search_cache()
            .get_or_fetch(key, || async move {
                let scored = match ModuleInfo::search_scored(query, max).await {
                    Ok(scored) => scored,
                    Err(e) => {
                        tracing::warn!(error = %e, query, "search_modules");
                        return Vec::new();
                    }
                };
                stream::iter(&scored)
                    .for_each_concurrent(CONCURRENCY_LIMIT, |(_, module)| module.resolve())
                    .await;
                scored
                    .iter()
                    .map(|(score, module)| module_entry(module, *score))
                    .collect()
            })
            .await;
        stamp_installed(&mut entries).await;
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    /// Serves `ListInstalledPackages() -> aa{sv}`: the packages declared in
    /// `environment.systemPackages`.
    ///
    /// Not cached: every call re-parses `package.nix` and runs one batched
    /// `nix eval` covering every declared package to resolve its
    /// `pname`/`version`/`description` (see
    /// `install_package::list_installed_package`) — heavier than the
    /// name-only parse [`installed_sets`] uses elsewhere, but this method
    /// needs the full metadata to build display rows, not just an
    /// installed/not-installed bit. Every returned entry has `installed =
    /// true` by construction, so no [`stamp_installed`] call is made — the
    /// flag is still set before [`dedup_by_group`] runs, since dedup uses it
    /// to elect each group's representative.
    ///
    /// # Returns
    /// One row per declared package (see `entry::AppEntry`), deduplicated by
    /// [`dedup_by_group`]. An empty array when nothing has ever been
    /// installed through the daemon (no `package.nix` yet) — not an error.
    ///
    /// # Errors
    /// [`zbus::fdo::Error::Failed`] when the blocking parse/eval task
    /// panics or is cancelled, or when the underlying parse/`nix eval` call
    /// itself fails (malformed `package.nix`, broken attribute) — see
    /// [`to_result`].
    async fn list_installed_packages(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let pkgs = tokio::task::spawn_blocking(|| {
            install_package::list_installed_package(crate::config_dir::config_dir())
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("list_installed_packages: join: {e}")))?;
        let pkgs = to_result(pkgs, "list_installed_packages")?;
        let mut entries: Vec<AppEntry> = pkgs.iter().map(|p| package_entry(p, 0)).collect();
        for entry in &mut entries {
            entry.installed = true;
        }
        let entries = dedup_by_group(entries);
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    /// Serves `ListInstalledModules() -> aa{sv}`: the Modulix modules
    /// currently enabled in the system configuration.
    ///
    /// Not cached. Enabled module names are read from the on-disk
    /// configuration (cheap, local parse) and resolved against the module
    /// index (`ModuleInfo::new`, itself index-cached process-wide); a name
    /// enabled locally but since removed from the remote index is silently
    /// dropped. Every resolved module then goes through [`resolve_all`]: up
    /// to [`CONCURRENCY_LIMIT`] Flathub/`metadata.json` fetches in flight at
    /// once, each a network round-trip, so this call's latency scales with
    /// the number of installed modules divided by [`CONCURRENCY_LIMIT`].
    ///
    /// # Returns
    /// One row per enabled, still-indexed module (see `entry::AppEntry`),
    /// each with `installed = true`. An empty array when none are enabled —
    /// not an error.
    ///
    /// # Errors
    /// [`zbus::fdo::Error::Failed`] when listing the enabled module names
    /// itself fails (see [`to_result`]) — a per-module resolution failure is
    /// not one of these, it just drops that module from the reply.
    async fn list_installed_modules(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let modules = to_result(
            install_module::list_installed_modules(crate::config_dir::config_dir()).await,
            "list_installed_modules",
        )?;
        resolve_all(&modules).await;
        let entries: Vec<AppEntry> = modules
            .iter()
            .map(|m| AppEntry {
                installed: true,
                ..module_entry(m, 0)
            })
            .collect();
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    /// Serves `ListModulePlugins(s module) -> aa{sv}`: the plugins a module
    /// exposes (e.g. browser extensions, editor plugins), each with its
    /// current enabled state.
    ///
    /// The plugin listing itself is served from [`PLUGINS_CACHE`] when
    /// fresh for `module` ([`PLUGINS_CACHE_TTL`], 5 minutes; single-flight
    /// across concurrent callers for the same module). On a miss, the whole
    /// nixpkgs plugin namespace the module declares is evaluated in one
    /// `nix eval` (see `ModuleInfo::list_plugins` /
    /// `list_plugins_in_namespace`), which is itself cached forever,
    /// process-wide, once it succeeds. `PluginEntry::installed` is stamped
    /// fresh after every read (see [`stamp_plugins_installed`]) from a
    /// separate, much shorter-lived cache ([`INSTALLED_CACHE_TTL`], 5s), so
    /// the (expensive) plugin catalogue can stay cached far longer than the
    /// (cheap, config-derived) install flags.
    ///
    /// # Parameters
    /// * `module` - module key as used in the configuration.
    ///
    /// # Returns
    /// One row per plugin (`name`, `description`, `installed`; see
    /// `entry::PluginEntry`). Empty for a module with no plugin namespace,
    /// and — since the underlying resolution failure is swallowed rather
    /// than propagated — empty for an unknown `module` key too, not an
    /// error.
    ///
    /// # Errors
    /// Never returns `Err`: a failure to resolve `module` or to evaluate its
    /// plugin namespace is logged at `warn` and yields an empty result
    /// instead.
    async fn list_module_plugins(&self, module: &str) -> zbus::fdo::Result<Vec<Dict>> {
        let key = module.to_string();
        let module_owned = module.to_string();
        let mut plugins = plugins_cache()
            .get_or_fetch(key, || async move {
                let result = async {
                    let module = ModuleInfo::new(&module_owned).await?;
                    module.list_plugins().await
                }
                .await;
                match result {
                    Ok(plugins) => plugins
                        .into_iter()
                        .map(|p| PluginEntry {
                            name: p.name,
                            description: p.description,
                            installed: false,
                            module: None,
                        })
                        .collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, module = %module_owned, "list_module_plugins");
                        Vec::new()
                    }
                }
            })
            .await;
        stamp_plugins_installed(module, &mut plugins).await;
        Ok(plugins.into_iter().map(PluginEntry::into_dict).collect())
    }

    /// Serves `ListInstalledPlugins(s module) -> aa{sv}`: every plugin
    /// currently installed, across one or all enabled modules — the listing
    /// behind the "Installed" page's Add-ons section, as opposed to
    /// [`Self::list_module_plugins`] which lists a single module's whole
    /// catalogue (installed or not).
    ///
    /// Runs no `nix eval`: the pairs come from [`installed_plugins_all`]
    /// (config-derived, 5s-cached, one `module.nix` read for every module at
    /// once via `modulix_core_utils::install_module::list_installed_plugins`),
    /// filtered down to `module` when non-empty; the description is a
    /// best-effort read of an existing cache entry ([`plugins_cache`]`.get_fresh`,
    /// never a fetch), so an uncached module's plugins are reported with an
    /// empty description rather than triggering the (expensive) catalogue
    /// evaluation.
    ///
    /// # Parameters
    /// * `module` - when non-empty, restrict the listing to this module's
    ///   installed plugins; when empty, cover every currently enabled
    ///   module.
    ///
    /// # Returns
    /// One row per installed plugin (`name`, `description`, `installed:
    /// true`, `module`; see `entry::PluginEntry`), `description` empty when
    /// the module's plugin catalogue is not already cached.
    ///
    /// # Errors
    /// Never returns `Err`.
    async fn list_installed_plugins(&self, module: &str) -> zbus::fdo::Result<Vec<Dict>> {
        let all = installed_plugins_all().await;

        let mut entries = Vec::new();
        for (m, name) in all.iter() {
            if !module.is_empty() && m != module {
                continue;
            }
            let description = plugins_cache()
                .get_fresh(m)
                .and_then(|plugins| plugins.iter().find(|p| &p.name == name).cloned())
                .map(|p| p.description)
                .unwrap_or_default();
            entries.push(PluginEntry {
                name: name.clone(),
                description,
                installed: true,
                module: Some(m.clone()),
            });
        }
        Ok(entries.into_iter().map(PluginEntry::into_dict).collect())
    }

    /// Serves `GetAppEnrichment(as app_ids) -> a{sa{sv}}`: Flathub-sourced
    /// metadata (long description, screenshots, icon URL, license) that the
    /// nix package index does not carry.
    ///
    /// Each id is cached individually in [`ENRICH_CACHE`]
    /// ([`ENRICH_CACHE_TTL`], 1h; `None` cached too, so an id known to have
    /// no Flathub listing is not retried every call). Unlike
    /// `search_packages`/`search_modules`/`packages_for_app_id`, this method
    /// does not go through `FlightCache::get_or_fetch`: it checks
    /// `enrich_cache().get_fresh` per id up front, fetches every still-missing
    /// id concurrently (`FlatpakInfo::new`, an HTTP round-trip per id, up to
    /// [`CONCURRENCY_LIMIT`] in flight at once), then inserts each result
    /// before assembling the reply. `get_fresh` never reserves a key the way
    /// `get_or_fetch`'s `OnceCell` slot does, so this batching is
    /// single-flight only *within* one call's own `missing` list: two
    /// overlapping top-level calls that both miss the same id can each
    /// trigger their own Flathub fetch for it.
    ///
    /// # Parameters
    /// * `app_ids` - AppStream component ids to enrich, in one round-trip.
    ///
    /// # Returns
    /// One entry per id in `app_ids` that has enrichment to report (see
    /// `entry::EnrichEntry`), keyed by id; an id with no Flathub listing, or
    /// whose fetch failed, is simply absent from the map rather than mapped
    /// to an empty row.
    ///
    /// # Errors
    /// Never returns `Err`: a failed fetch for one id just leaves that id
    /// out of the reply.
    async fn get_app_enrichment(
        &self,
        app_ids: Vec<&str>,
    ) -> zbus::fdo::Result<HashMap<String, Dict>> {
        let missing: Vec<String> = app_ids
            .iter()
            .filter(|id| enrich_cache().get_fresh(&id.to_string()).is_none())
            .map(|id| id.to_string())
            .collect();

        if !missing.is_empty() {
            let fetched: Vec<(String, Option<EnrichEntry>)> = stream::iter(missing)
                .map(|id| async move {
                    let entry = fetch_enrichment(&id).await;
                    (id, entry)
                })
                .buffer_unordered(CONCURRENCY_LIMIT)
                .collect()
                .await;
            for (id, entry) in fetched {
                enrich_cache().insert(id, entry);
            }
        }

        let mut out = HashMap::new();
        for id in app_ids {
            if let Some(Some(entry)) = enrich_cache().get_fresh(&id.to_string()) {
                out.insert(id.to_string(), entry.into_dict());
            }
        }
        Ok(out)
    }

    /// Serves `GetPackageLicenses(as attrs) -> a{ss}`: the SPDX expression
    /// (or `LicenseRef-*`) per nixpkgs attribute, for the attributes that
    /// have one.
    ///
    /// Costs one `nix eval` per uncached attribute
    /// ([`package_info::license_for_package`]), so at most
    /// [`MAX_LICENSE_EVALS`] of them are evaluated per call — the rest are
    /// served from [`LICENSE_CACHE`] ([`ENRICH_CACHE_TTL`], 1h) or simply
    /// omitted. In practice a store only asks for the license of the app
    /// whose details page is open. Cached the same way as
    /// [`Store::get_app_enrichment`] (per-attribute `get_fresh` check, then
    /// a concurrent `buffer_unordered` fetch of the misses, up to
    /// [`CONCURRENCY_LIMIT`] `nix eval`s in flight at once, then
    /// `insert`), with the same caveat: single-flight only within one call's
    /// own miss list, not across concurrent top-level calls.
    ///
    /// # Parameters
    /// * `attrs` - nixpkgs attribute paths (e.g. `firefox`) to resolve.
    ///
    /// # Returns
    /// One entry per attribute in `attrs` that has a resolvable license,
    /// keyed by attribute. An attribute past the [`MAX_LICENSE_EVALS`]
    /// budget, with no license metadata, or whose evaluation failed, is
    /// simply absent from the map rather than mapped to an empty string.
    ///
    /// # Errors
    /// Never returns `Err`: an evaluation failure for one attribute just
    /// leaves that attribute out of the reply.
    async fn get_package_licenses(
        &self,
        attrs: Vec<&str>,
    ) -> zbus::fdo::Result<HashMap<String, String>> {
        let missing: Vec<String> = attrs
            .iter()
            .filter(|attr| license_cache().get_fresh(&attr.to_string()).is_none())
            .take(MAX_LICENSE_EVALS)
            .map(|attr| attr.to_string())
            .collect();

        if !missing.is_empty() {
            let fetched: Vec<(String, Option<String>)> = stream::iter(missing)
                .map(|attr| async move {
                    let license = package_info::license_for_package(&attr).await;
                    (attr, license)
                })
                .buffer_unordered(CONCURRENCY_LIMIT)
                .collect()
                .await;
            for (attr, license) in fetched {
                license_cache().insert(attr, license);
            }
        }

        let mut out = HashMap::new();
        for attr in attrs {
            if let Some(Some(license)) = license_cache().get_fresh(&attr.to_string()) {
                out.insert(attr.to_string(), license);
            }
        }
        Ok(out)
    }

    /// Serves `PackagesForAppId(s app_id) -> aa{sv}`: every installable
    /// source of a desktop app (nix attribute variants and/or a Modulix
    /// module), for the "alternate sources" / version-selector UI.
    ///
    /// Cached in [`ALT_CACHE`] ([`ALT_CACHE_TTL`], 5 minutes; single-flight
    /// across concurrent callers for the same grouping key) via
    /// [`build_alt_entries`], keyed *before* dedup on the resolved group id
    /// (the curated app-id, or `app_id` itself when uncurated) — see there
    /// for the module-lookup timeout and the live `nix search` fallback for
    /// pname-only groups. `AppEntry::installed` is stamped fresh after the
    /// cache read, then rows are sorted by [`alt_sort_key`] (modules first,
    /// then packages by variant rank, ties by name).
    ///
    /// # Parameters
    /// * `app_id` - AppStream component id (e.g. `org.mozilla.firefox`),
    ///   or the `.desktop`-suffixed id some callers pass instead — when the
    ///   curated table has nothing for `app_id` as given, the `.desktop`
    ///   suffix is stripped and the lookup retried once before falling back
    ///   to treating `app_id` itself as the pname-search grouping key.
    ///
    /// # Returns
    /// One row per candidate source (see `entry::AppEntry`): the matching
    /// Modulix module(s) first, then the nix package variants. Empty when
    /// no package or module maps to `app_id` — not an error.
    ///
    /// # Errors
    /// Never returns `Err`: an unresolvable id, a module-lookup timeout, or
    /// a failed live `nix search` all degrade to fewer or zero rows rather
    /// than a fault (see [`build_alt_entries`]).
    async fn packages_for_app_id(&self, app_id: &str) -> zbus::fdo::Result<Vec<Dict>> {
        let mut table_attrs = package_info::packages_for_app_id(app_id);
        let gid: String = if table_attrs.is_empty() {
            match app_id.strip_suffix(".desktop") {
                Some(stripped) => {
                    let retry = package_info::packages_for_app_id(stripped);
                    if !retry.is_empty() {
                        table_attrs = retry;
                        stripped.to_string()
                    } else {
                        app_id.to_string()
                    }
                }
                None => app_id.to_string(),
            }
        } else {
            app_id.to_string()
        };

        let key = gid.clone();
        let mut entries = alt_cache()
            .get_or_fetch(key, || build_alt_entries(gid, table_attrs))
            .await;
        stamp_installed(&mut entries).await;
        entries.sort_by(|a, b| alt_sort_key(a).cmp(&alt_sort_key(b)));
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    /// Serves `ListOutdatedInputs(b force_refresh) -> aa{sv}`: the direct
    /// flake inputs whose upstream revision has moved past the one pinned in
    /// `flake.lock` — what an `UpdateSystem` call would refresh.
    ///
    /// Served from [`UPDATE_CACHE`] when fresh ([`UPDATE_CACHE_TTL`], 1h;
    /// single-flight across concurrent callers, same [`FlightCache`] pattern
    /// as every other cache in this module). On a miss, probes every direct
    /// input's upstream metadata via
    /// [`modulix_core_utils::update::outdated_inputs`] — one `nix flake
    /// metadata --refresh` network round-trip per input, sequential, each
    /// bounded by that function's own per-input timeout; an input whose
    /// probe fails is silently absent from the result rather than failing
    /// the whole call (see that function's docs). A completed `UpdateSystem`
    /// call invalidates this cache early (see [`invalidate_updates`]).
    ///
    /// # Parameters
    /// * `force_refresh` - when `true`, the cache is cleared before the
    ///   read, forcing a fresh probe of every input regardless of
    ///   [`UPDATE_CACHE_TTL`] — what GNOME Software's `refresh_metadata_async`
    ///   uses for an explicit user-requested refresh; a routine background
    ///   check passes `false` and rides the cache.
    ///
    /// # Returns
    /// One row per outdated direct input (see `entry::InputEntry`). An empty
    /// array when every input is already current, or when `flake.lock`
    /// cannot be read/parsed — not an error either way (see
    /// [`modulix_core_utils::update::outdated_inputs`]).
    ///
    /// # Errors
    /// Never returns `Err`.
    async fn list_outdated_inputs(&self, force_refresh: bool) -> zbus::fdo::Result<Vec<Dict>> {
        if force_refresh {
            update_cache().clear();
        }
        let entries = update_cache()
            .get_or_fetch((), || async {
                let inputs =
                    modulix_core_utils::update::outdated_inputs(crate::config_dir::config_dir())
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "list_outdated_inputs");
                            Vec::new()
                        });
                Arc::new(inputs.into_iter().map(InputEntry::from).collect())
            })
            .await;
        Ok(entries.iter().cloned().map(InputEntry::into_dict).collect())
    }
}

/// Fetches Flathub enrichment for one app-id. No caching — [`Store::get_app_enrichment`]
/// owns that via [`ENRICH_CACHE`].
///
/// # Parameters
/// * `app_id` - AppStream component id to fetch Flathub metadata for.
///
/// # Returns
/// The enrichment entry, with `description`/`icon` set to `None` rather
/// than an empty string when Flathub reports none, and `icon_name` taken
/// from the curated table (not from Flathub) via [`icon_name_for_app_id`].
/// `None` when `app_id` has no Flathub listing or the request failed — this
/// function performs one HTTP round-trip ([`FlatpakInfo::new`]) and never
/// reports why it failed, a missing enrichment being a normal outcome.
async fn fetch_enrichment(app_id: &str) -> Option<EnrichEntry> {
    let info = FlatpakInfo::new(app_id).await.ok()?;
    let description = info.description();
    let icon = info.icon();
    Some(EnrichEntry {
        description: (!description.is_empty()).then(|| description.to_string()),
        screenshots: collect_screenshots(info.screenshots()),
        icon: (!icon.is_empty()).then(|| icon.to_string()),
        icon_name: icon_name_for_app_id(app_id).map(str::to_string),
        license: info.license(),
    })
}

/// All installable variants of a group, for the `alternate-of` query. The
/// argument is the grouping key, i.e. [`AppEntry::group_id`]:
///
/// - a Modulix module targeting this app-id, if any, always comes first
///   ([`module_rows_for_app_id`]);
/// - **app-id group** (in the curated table): the nix attributes mapped to
///   that AppStream id (`firefox`, `firefox-bin`, …) — cheap table lookup;
/// - **pname group** (no AppStream id, e.g. `nvtop`): a live `nix search` for
///   the pname, keeping every attribute whose pname matches. Skipped when the
///   id looks like a dotted AppStream id (a pname search could never match
///   one), so a plain Flatpak app never triggers a live `nix search`.
///
/// # Parameters
/// * `gid` - the grouping key, i.e. [`AppEntry::group_id`] — an AppStream
///   app-id or a bare pname.
/// * `table_attrs` - the curated nix attributes for `gid`, or an empty
///   slice when `gid` is not in the curated app-id table (in which case the
///   pname-group branch may run instead).
///
/// # Returns
/// The candidate rows for `gid`, unsorted and un-stamped for `installed`
/// (both done by the caller, [`Store::packages_for_app_id`]); possibly
/// empty when `gid` maps to no module, no curated attribute and no live
/// pname match.
///
/// # Errors
/// Never returns `Err`: the module lookup times out silently
/// ([`MODULE_LOOKUP_TIMEOUT`]) and a failed live `nix search`
/// ([`MAX_VARIANT_SEARCH`] cap) degrades to no pname-group rows, via
/// `unwrap_or_default`.
async fn build_alt_entries(gid: String, table_attrs: &'static [&'static str]) -> Vec<AppEntry> {
    let mut entries = module_rows_for_app_id(&gid, MODULE_LOOKUP_TIMEOUT).await;

    if !table_attrs.is_empty() {
        let flatpak_preferred = package_info::is_flatpak_preferred(&gid);
        let base = base_attr(table_attrs);
        entries.extend(table_attrs.iter().copied().map(|attr| AppEntry {
            name: attr.to_string(),
            base_name: icon_base_name(attr).unwrap_or_else(|| attr.to_string()),
            pname: attr.to_string(),
            app_name: package_info::name_for_package(attr).map(str::to_string),
            summary: String::new(),
            version: String::new(),
            app_id: Some(gid.clone()),
            group_id: Some(gid.clone()),
            icon: None,
            icon_name: package_info::icon_name_for_package(attr).map(str::to_string),
            kind: "package",
            flatpak_preferred,
            variant_rank: variant_rank(attr, base),
            score: 0,
            installed: false,
        }));
    } else if !gid.contains('.') {
        let pkgs = NixPackage::search(&gid, MAX_VARIANT_SEARCH)
            .await
            .unwrap_or_default();
        entries.extend(pkgs.iter().filter(|p| p.display_name() == gid).map(|p| {
            let attr = p.package_name();
            AppEntry {
                name: attr.to_string(),
                base_name: icon_base_name(attr).unwrap_or_else(|| attr.to_string()),
                pname: variant_label(&gid, attr),
                app_name: None,
                summary: p.summary().to_string(),
                version: p.version().to_string(),
                app_id: None,
                group_id: Some(gid.clone()),
                icon: None,
                icon_name: p.icon_name().map(str::to_string),
                kind: "package",
                flatpak_preferred: false,
                variant_rank: variant_rank(attr, &gid),
                score: 0,
                installed: false,
            }
        }));
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn packages_for_app_id_serializes() {
        let entries = build_alt_entries(
            "org.mozilla.firefox".to_string(),
            package_info::packages_for_app_id("org.mozilla.firefox"),
        )
        .await;
        let _dicts: Vec<Dict> = entries.into_iter().map(AppEntry::into_dict).collect();
    }

    fn entry(name: &str, kind: &'static str) -> AppEntry {
        AppEntry {
            name: name.to_string(),
            base_name: name.to_string(),
            pname: name.to_string(),
            app_name: None,
            summary: String::new(),
            version: String::new(),
            app_id: None,
            group_id: None,
            icon: None,
            icon_name: None,
            kind,
            flatpak_preferred: false,
            variant_rank: 0,
            score: 0,
            installed: false,
        }
    }

    #[test]
    fn installed_sets_match_per_kind() {
        let sets = InstalledSets {
            packages: ["htop".to_string()].into_iter().collect(),
            modules: ["audio".to_string()].into_iter().collect(),
        };
        assert!(sets.contains(&entry("htop", "package")));
        assert!(sets.contains(&entry("audio", "module")));
        assert!(!sets.contains(&entry("audio", "package")));
        assert!(!sets.contains(&entry("htop", "module")));
    }

    /// The client must be able to tell "not installed" from "absent field"
    /// (see `AppEntry::installed`), so `false` has to be on the wire too.
    #[test]
    fn into_dict_always_carries_installed() {
        let dict = entry("htop", "package").into_dict();
        assert!(dict.contains_key("installed"));
    }
}
