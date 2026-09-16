//! `org.modulix.Store1`: the daemon's read-only interface — search, listings,
//! metadata enrichment, alternate sources. Replaces the in-process
//! `gnome-software-plugin/backend` FFI layer; every store (GNOME Software
//! today, others tomorrow) becomes a thin D-Bus client of this interface.
//!
//! Payloads are typed `a{sv}` (see [`entry`]), not JSON: the daemon stays
//! neutral (no GNOME-Software-specific number crosses the bus) and every
//! client gets structural typing for free.

pub mod entry;

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use modulix_core_utils::module_info::ModuleInfo;
use modulix_core_utils::package_info::{self, NixPackage};
use modulix_core_utils::{
    AppInfoGui, AppInfoMinimal, CONFIG_DIRECTORY, FlatpakInfo, install_module, install_package,
    package_index,
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
        let entries = search_cache()
            .get_or_fetch(key, || async move {
                match NixPackage::search_scored(query, max).await {
                    Ok(pkgs) => {
                        // Score before dedup: `dedup_by_group` keeps the first
                        // entry per group and relies on relevance order.
                        let scored: Vec<AppEntry> = pkgs
                            .iter()
                            .map(|(score, pkg)| package_entry(pkg, *score))
                            .collect();
                        dedup_by_group(scored)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, query, "search_packages");
                        Vec::new()
                    }
                }
            })
            .await;
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    async fn search_modules(&self, query: &str, max: u32) -> zbus::fdo::Result<Vec<Dict>> {
        let key = ("mod", query.to_string(), max);
        let entries = search_cache()
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
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    async fn list_installed_packages(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let pkgs = tokio::task::spawn_blocking(|| {
            install_package::list_installed_package(CONFIG_DIRECTORY)
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("list_installed_packages: join: {e}")))?;
        let pkgs = to_result(pkgs, "list_installed_packages")?;
        let entries = dedup_by_group(pkgs.iter().map(|p| package_entry(p, 0)).collect());
        Ok(entries.into_iter().map(AppEntry::into_dict).collect())
    }

    async fn list_installed_modules(&self) -> zbus::fdo::Result<Vec<Dict>> {
        let modules = to_result(
            install_module::list_installed_modules(CONFIG_DIRECTORY).await,
            "list_installed_modules",
        )?;
        resolve_all(&modules).await;
        let entries: Vec<AppEntry> = modules.iter().map(|m| module_entry(m, 0)).collect();
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
}
