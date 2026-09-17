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
    AppEntry, Dict, EnrichEntry, PluginEntry, alt_sort_key, base_attr, collect_screenshots,
    dedup_by_group, icon_base_name, icon_name_for_app_id, module_entry, module_rows_for_app_id,
    package_entry, variant_label, variant_rank,
};

/// Cap shared by every cache in this module: one entry per distinct query/
/// app-id ever seen would otherwise grow unbounded over the daemon's
/// lifetime — see [`crate::cache::FlightCache`].
const CACHE_CAP: usize = 512;

/// Cap on in-flight `resolve()` / enrichment fetches per batch.
const CONCURRENCY_LIMIT: usize = 8;

const SEARCH_CACHE_TTL: Duration = Duration::from_secs(60);
const ALT_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const ENRICH_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const PLUGINS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// Short on purpose: the configuration can also change behind the daemon's
/// back (the `mx` CLI, a hand-edited `package.nix`). Writes that go *through*
/// the daemon don't wait for it — they call [`invalidate_installed`].
const INSTALLED_CACHE_TTL: Duration = Duration::from_secs(5);

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

/// `(kind, query, max)`, where `kind` is `"pkg"` or `"mod"`.
type SearchKey = (&'static str, String, u32);

static SEARCH_CACHE: OnceLock<FlightCache<SearchKey, Vec<AppEntry>>> = OnceLock::new();
static ALT_CACHE: OnceLock<FlightCache<String, Vec<AppEntry>>> = OnceLock::new();
static ENRICH_CACHE: OnceLock<FlightCache<String, Option<EnrichEntry>>> = OnceLock::new();
static PLUGINS_CACHE: OnceLock<FlightCache<String, Vec<PluginEntry>>> = OnceLock::new();
static LICENSE_CACHE: OnceLock<FlightCache<String, Option<String>>> = OnceLock::new();
static INSTALLED_CACHE: OnceLock<FlightCache<(), Arc<InstalledSets>>> = OnceLock::new();

fn search_cache() -> &'static FlightCache<SearchKey, Vec<AppEntry>> {
    SEARCH_CACHE.get_or_init(|| FlightCache::new(SEARCH_CACHE_TTL, CACHE_CAP))
}

fn alt_cache() -> &'static FlightCache<String, Vec<AppEntry>> {
    ALT_CACHE.get_or_init(|| FlightCache::new(ALT_CACHE_TTL, CACHE_CAP))
}

fn enrich_cache() -> &'static FlightCache<String, Option<EnrichEntry>> {
    ENRICH_CACHE.get_or_init(|| FlightCache::new(ENRICH_CACHE_TTL, CACHE_CAP))
}

fn plugins_cache() -> &'static FlightCache<String, Vec<PluginEntry>> {
    PLUGINS_CACHE.get_or_init(|| FlightCache::new(PLUGINS_CACHE_TTL, CACHE_CAP))
}

/// Licenses change only when nixpkgs does: same generous TTL as enrichment.
fn license_cache() -> &'static FlightCache<String, Option<String>> {
    LICENSE_CACHE.get_or_init(|| FlightCache::new(ENRICH_CACHE_TTL, CACHE_CAP))
}

/// What the system configuration currently declares: nixpkgs attributes in
/// `environment.systemPackages`, and enabled `mx.*` modules.
///
/// Both listings are pure parsing of the config files — no `nix eval`, no
/// network — which is what makes it affordable to consult them on the search
/// path (see [`installed_sets`]).
#[derive(Default)]
struct InstalledSets {
    packages: HashSet<String>,
    modules: HashSet<String>,
}

impl InstalledSets {
    fn contains(&self, entry: &AppEntry) -> bool {
        if entry.kind == "module" {
            self.modules.contains(&entry.name)
        } else {
            self.packages.contains(&entry.name)
        }
    }
}

fn installed_cache() -> &'static FlightCache<(), Arc<InstalledSets>> {
    // One key, so `cap` only has to be non-zero.
    INSTALLED_CACHE.get_or_init(|| FlightCache::new(INSTALLED_CACHE_TTL, 1))
}

/// The cached installed sets. Behind an `Arc`: one search stamps hundreds of
/// entries against the same snapshot, and cloning the sets each time would
/// dwarf the lookup itself.
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
async fn stamp_installed(entries: &mut [AppEntry]) {
    let sets = installed_sets().await;
    for entry in entries.iter_mut() {
        entry.installed = sets.contains(entry);
    }
}

/// Drops the cached installed sets, so the next read re-parses the
/// configuration. Called by [`crate::daemon`] after a successful install or
/// uninstall: without it the store would keep showing the pre-install state
/// for up to [`INSTALLED_CACHE_TTL`].
pub fn invalidate_installed() {
    installed_cache().clear();
}

/// Resolve every module concurrently (bounded) instead of one at a time —
/// `resolve()` does a network round-trip per module.
async fn resolve_all(modules: &[ModuleInfo]) {
    stream::iter(modules)
        .for_each_concurrent(CONCURRENCY_LIMIT, |module| module.resolve())
        .await;
}

fn to_result<T>(r: Result<T, impl std::fmt::Display>, ctx: &str) -> zbus::fdo::Result<T> {
    r.map_err(|e| {
        tracing::warn!(error = %e, "{ctx}");
        zbus::fdo::Error::Failed(format!("{ctx}: {e}"))
    })
}

/// The `org.modulix.Store1` interface implementation.
#[derive(Default)]
pub struct Store;

#[zbus::interface(name = "org.modulix.Store1")]
impl Store {
    /// Whether the on-disk nix package index is currently servable. Search
    /// still works when `false` (it falls back to a live `nix search`), just
    /// slower on a cold cache.
    #[zbus(property)]
    async fn index_ready(&self) -> bool {
        package_index::is_ready().await
    }

    async fn search_packages(&self, query: &str, max: u32) -> zbus::fdo::Result<Vec<Dict>> {
        let key = ("pkg", query.to_string(), max);
        // Cached *before* dedup: which variant represents a group depends on
        // what is installed, which the cached value must not freeze in.
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
        // Relevance order is still what `dedup_by_group` falls back on when no
        // variant of a group is installed.
        let entries = dedup_by_group(entries);
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

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

    async fn list_installed_packages(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let pkgs = tokio::task::spawn_blocking(|| {
            install_package::list_installed_package(crate::config_dir::config_dir())
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("list_installed_packages: join: {e}")))?;
        let pkgs = to_result(pkgs, "list_installed_packages")?;
        // Everything on this path is installed by definition — no need for
        // `stamp_installed`, but the flag still has to be set before
        // `dedup_by_group` reads it.
        let mut entries: Vec<AppEntry> = pkgs.iter().map(|p| package_entry(p, 0)).collect();
        for entry in &mut entries {
            entry.installed = true;
        }
        let entries = dedup_by_group(entries);
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

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

    async fn list_module_plugins(&self, module: &str) -> zbus::fdo::Result<Vec<Dict>> {
        let key = module.to_string();
        let module_owned = module.to_string();
        let plugins = plugins_cache()
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
                        })
                        .collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, module = %module_owned, "list_module_plugins");
                        Vec::new()
                    }
                }
            })
            .await;
        Ok(plugins.into_iter().map(PluginEntry::into_dict).collect())
    }

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

    /// SPDX expression per nixpkgs attribute, for the attributes that have
    /// one (others are simply absent from the reply).
    ///
    /// Costs one `nix eval` per uncached attribute
    /// ([`package_info::license_for_package`]), so at most
    /// [`MAX_LICENSE_EVALS`] of them are evaluated per call — the rest are
    /// served from the cache or omitted. In practice a store only asks for
    /// the license of the app whose details page is open.
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

    async fn packages_for_app_id(&self, app_id: &str) -> zbus::fdo::Result<Vec<Dict>> {
        // A `.desktop`-suffixed id absent from the curated table is retried
        // bare (some callers pass the desktop-file id, not the AppStream id).
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
}

/// Fetches Flathub enrichment for one app-id. No caching — [`Store::get_app_enrichment`]
/// owns that via [`ENRICH_CACHE`].
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
        // A module and a package may share a name without sharing a state.
        assert!(!sets.contains(&entry("audio", "package")));
        assert!(!sets.contains(&entry("htop", "module")));
    }

    #[test]
    fn into_dict_always_carries_installed() {
        // The client must be able to tell "not installed" from "absent field"
        // (see `AppEntry::installed`), so `false` has to be on the wire too.
        let dict = entry("htop", "package").into_dict();
        assert!(dict.contains_key("installed"));
    }
}
