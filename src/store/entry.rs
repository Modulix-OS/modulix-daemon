//! Neutral, store-side app/plugin/enrichment entries and the pure logic
//! (dedup, variant labelling, icon-name fallback) that used to live in
//! `gnome-software-plugin/backend`. Unlike that crate this carries no
//! GNOME-Software-specific number: no `GnomeSoftware::SortKey`, no
//! `GsApp::match-value`. Those are computed client-side from `kind` +
//! `variant_rank` + `score` (see `gnome-software-plugin/plugin/src/gs-modulix-app.c`).

use std::collections::HashMap;

use modulix_core_utils::module_info::ModuleInfo;
use modulix_core_utils::package_info::{self, NixPackage};
use modulix_core_utils::{AppInfoGui, AppInfoMinimal, AppScreenshot, module_info};
use zbus::zvariant::{OwnedValue, Value};

pub type Dict = HashMap<String, OwnedValue>;

/// Wraps `v` as an [`OwnedValue`]. Infallible for every type used in this
/// module (only [`zvariant::Value::Fd`] conversion can fail).
fn ov<T>(v: T) -> OwnedValue
where
    Value<'static>: From<T>,
{
    OwnedValue::try_from(Value::from(v)).expect("infallible for non-fd values")
}

#[derive(Clone, Debug)]
pub struct AppEntry {
    /// nixpkgs attribute (package) or module name — the install identifier.
    pub name: String,
    /// Themed-icon fallback: `name` with any known variant/edition suffix
    /// stripped, via [`icon_base_name`]; falls back to `name` unchanged.
    pub base_name: String,
    /// Human display name (pname / module display name).
    pub pname: String,
    /// Flatpak/AppStream display name, when the package matches a known app.
    pub app_name: Option<String>,
    pub summary: String,
    pub version: String,
    /// Canonical AppStream id used to deduplicate against Flatpak/AppStream.
    pub app_id: Option<String>,
    /// Stable grouping key: `app_id` when known, otherwise `pname`.
    pub group_id: Option<String>,
    pub icon: Option<String>,
    /// Themed icon name (`meta.mainProgram`).
    pub icon_name: Option<String>,
    pub kind: &'static str,
    pub flatpak_preferred: bool,
    /// Ordering rank of this entry within its variant group (see
    /// [`variant_rank`]). Meaningless across groups; combine with `kind` on
    /// the client to reproduce the Sources-popover order.
    pub variant_rank: i32,
    /// Raw search relevance (`0` outside search paths).
    pub score: u32,
}

impl AppEntry {
    pub fn into_dict(self) -> Dict {
        let mut d = Dict::new();
        d.insert("name".into(), ov(self.name));
        d.insert("base_name".into(), ov(self.base_name));
        d.insert("pname".into(), ov(self.pname));
        if let Some(app_name) = self.app_name {
            d.insert("app_name".into(), ov(app_name));
        }
        d.insert("summary".into(), ov(self.summary));
        d.insert("version".into(), ov(self.version));
        if let Some(app_id) = self.app_id {
            d.insert("app_id".into(), ov(app_id));
        }
        if let Some(group_id) = self.group_id {
            d.insert("group_id".into(), ov(group_id));
        }
        if let Some(icon) = self.icon {
            d.insert("icon".into(), ov(icon));
        }
        if let Some(icon_name) = self.icon_name {
            d.insert("icon_name".into(), ov(icon_name));
        }
        d.insert("kind".into(), ov(self.kind.to_string()));
        d.insert("flatpak_preferred".into(), ov(self.flatpak_preferred));
        d.insert("variant_rank".into(), ov(self.variant_rank));
        if self.score > 0 {
            d.insert("score".into(), ov(self.score));
        }
        d
    }
}

#[derive(Clone)]
pub struct PluginEntry {
    pub name: String,
    pub description: String,
}

impl PluginEntry {
    pub fn into_dict(self) -> Dict {
        let mut d = Dict::new();
        d.insert("name".into(), ov(self.name));
        d.insert("description".into(), ov(self.description));
        d
    }
}

/// One screenshot: `(caption, is_default, images)`; `images` is
/// `(url, width, height)` tuples. D-Bus signature `(sba(suu))`.
pub type ShotTuple = (String, bool, Vec<(String, u32, u32)>);

#[derive(Clone)]
pub struct EnrichEntry {
    pub description: Option<String>,
    pub screenshots: Vec<ShotTuple>,
    /// Flathub AppStream icon URL.
    pub icon: Option<String>,
    pub icon_name: Option<String>,
    /// SPDX expression (or an AppStream `LicenseRef-*`) for the app.
    pub license: Option<String>,
}

impl EnrichEntry {
    pub fn into_dict(self) -> Dict {
        let mut d = Dict::new();
        if let Some(description) = self.description {
            d.insert("description".into(), ov(description));
        }
        if !self.screenshots.is_empty() {
            d.insert("screenshots".into(), ov(self.screenshots));
        }
        if let Some(icon) = self.icon {
            d.insert("icon".into(), ov(icon));
        }
        if let Some(icon_name) = self.icon_name {
            d.insert("icon_name".into(), ov(icon_name));
        }
        if let Some(license) = self.license {
            d.insert("license".into(), ov(license));
        }
        d
    }
}

/// Convert the crate's borrowed [`AppScreenshot`] into owned `(sba(suu))` tuples.
pub fn collect_screenshots(shot: Option<AppScreenshot<'_>>) -> Vec<ShotTuple> {
    let Some(shot) = shot else {
        return Vec::new();
    };
    shot.screenshots
        .iter()
        .enumerate()
        .map(|(i, sized)| {
            (
                sized.caption.to_string(),
                i == shot.default,
                sized
                    .screenshot
                    .iter()
                    .map(|img| (img.url.to_string(), img.width, img.height))
                    .collect(),
            )
        })
        .collect()
}

pub fn package_entry(pkg: &NixPackage, score: u32) -> AppEntry {
    let app_id = pkg.id().map(str::to_string);
    let flatpak_preferred = app_id
        .as_deref()
        .map(package_info::is_flatpak_preferred)
        .unwrap_or(false);
    let name = pkg.package_name().to_string();
    let pname = pkg.display_name().to_string();
    // Group by app-id when known (firefox/firefox-bin), else by pname so
    // same-pname variants (nvtopPackages.amd/…) collapse to one row.
    let group_id = app_id
        .clone()
        .or_else(|| (!pname.is_empty()).then(|| pname.clone()))
        .or_else(|| Some(name.clone()));
    let rank = variant_rank(&name, &pname);
    let base_name = icon_base_name(&name).unwrap_or_else(|| name.clone());
    AppEntry {
        base_name,
        name,
        pname,
        app_name: pkg.app_name().map(str::to_string),
        summary: pkg.summary().to_string(),
        version: pkg.version().to_string(),
        app_id,
        group_id,
        icon: pkg.icon().map(str::to_string),
        icon_name: pkg.icon_name().map(str::to_string),
        kind: "package",
        flatpak_preferred,
        variant_rank: rank,
        score,
    }
}

/// Flatpak/AppStream display name for the app a Modulix module targets, via
/// the curated table also used for nix packages.
pub fn app_name_for_app_id(app_id: &str) -> Option<&'static str> {
    package_info::packages_for_app_id(app_id)
        .first()
        .and_then(|attr| package_info::name_for_package(attr))
}

/// Same table-lookup trick as [`app_name_for_app_id`], for the themed icon name.
pub fn icon_name_for_app_id(app_id: &str) -> Option<&'static str> {
    package_info::packages_for_app_id(app_id)
        .first()
        .and_then(|attr| package_info::icon_name_for_package(attr))
}

/// Build a module entry. Call after `resolve()` so `icon()` is populated.
pub fn module_entry(module: &ModuleInfo, score: u32) -> AppEntry {
    let app_id = module.id().map(str::to_string);
    let pname = app_id
        .as_deref()
        .and_then(app_name_for_app_id)
        .map(str::to_string)
        .unwrap_or_else(|| module.display_name().to_string());
    AppEntry {
        name: module.package_name().to_string(),
        base_name: module.package_name().to_string(),
        pname,
        app_name: None,
        summary: module.summary().to_string(),
        version: String::new(),
        // Modules never group by pname (only by their own app-id, if any).
        group_id: app_id.clone(),
        app_id,
        icon: module.icon().map(str::to_string),
        icon_name: module.icon_name().map(str::to_string),
        kind: "module",
        flatpak_preferred: false,
        variant_rank: 0,
        score,
    }
}

/// Collapse nix variants that share a grouping key into one entry: same-app-id
/// variants (`firefox`, `firefox-bin` → one "Firefox") **and** same-pname
/// variants (`nvtopPackages.amd`, `nvtopPackages.nvidia` → one "nvtop"). The
/// first entry per key wins (search results are relevance-sorted); entries
/// without a `group_id` are kept individually. The dropped variants stay
/// reachable through [`crate::store::Store::packages_for_app_id`].
pub fn dedup_by_group(entries: Vec<AppEntry>) -> Vec<AppEntry> {
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        .filter(|e| match &e.group_id {
            Some(id) => seen.insert(id.clone()),
            None => true,
        })
        .collect()
}

// ─── Variant labelling ────────────────────────────────────────────────────────

/// Distinguishing suffix of a nix attribute within its pname group, used to
/// label stacked variants: `nvtopPackages.amd` → `amd`, `nvtop-amd` → `amd`,
/// `firefox-esr` → `esr`. `None` when the attribute carries no extra suffix
/// over the bare pname (e.g. `nvtop`/`nvtop`).
pub fn variant_suffix(attr: &str, pname: &str) -> Option<String> {
    if let Some((_, tail)) = attr.rsplit_once('.') {
        return (!tail.is_empty()).then(|| tail.to_string());
    }
    if let Some(rest) = attr.strip_prefix(pname) {
        let rest = rest.trim_start_matches(['-', '_']);
        return (!rest.is_empty()).then(|| rest.to_string());
    }
    (attr != pname).then(|| attr.to_string())
}

/// Display label for a stacked pname variant: `pname (suffix)` or the bare
/// `pname` when there is no distinguishing suffix.
pub fn variant_label(pname: &str, attr: &str) -> String {
    match variant_suffix(attr, pname) {
        Some(suffix) => format!("{pname} ({suffix})"),
        None => pname.to_string(),
    }
}

/// The "base" nix attribute of a group of variants sharing one app-id/pname:
/// the shortest attribute, lexicographic order breaking ties.
pub fn base_attr<'a>(attrs: &[&'a str]) -> &'a str {
    attrs
        .iter()
        .copied()
        .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
        .unwrap_or("")
}

/// Ordering rank of a nix attribute within its variant group. Known suffixes
/// follow the upstream convention (`-bin`, `-beta`, `-nightly`,
/// `-devedition`, `-esr`, each with an optional `-bin` pairing); the base
/// attribute (no suffix over `base`) ranks first, `-unwrapped` near the end,
/// and any other suffix last (alphabetically, via a final `(rank, name)` sort).
pub fn variant_rank(attr: &str, base: &str) -> i32 {
    match variant_suffix(attr, base).as_deref() {
        None => 20,
        Some("bin") => 10,
        Some("beta-bin") => 30,
        Some("beta") => 40,
        Some("nightly-bin") => 50,
        Some("nightly") => 60,
        Some("devedition-bin") => 70,
        Some("devedition") => 80,
        Some("esr-bin") => 90,
        Some("esr") => 100,
        Some("unwrapped") => 800,
        Some(_) => 900,
    }
}

/// Attribute suffixes that name another build or edition of the same
/// program. Used only for the themed-icon fallback ([`icon_base_name`]) —
/// never for identity inheritance (`app_id`/`app_name`).
const ICON_BASE_SUFFIXES: &[&str] = &[
    // build / channel
    "bin",
    "unwrapped",
    "wrapped",
    "stable",
    "unstable",
    "git",
    "nightly",
    "beta",
    "dev",
    "devedition",
    "esr",
    "fhs",
    "appimage",
    "electron",
    "gtk",
    "gtk3",
    "gtk4",
    "qt",
    "qt5",
    "qt6",
    "wayland",
    "x11",
    "static", // edition
    "studio",
    "free",
    "full",
    "lite",
    "minimal",
    "pro",
    "cli",
    "gui",
    "desktop",
    "community",
    "enterprise",
];

/// Strip trailing `-<suffix>`/`_<suffix>` tokens in [`ICON_BASE_SUFFIXES`]
/// from a nix attribute, e.g. `davinci-resolve-studio` → `davinci-resolve`.
/// `None` when no suffix was stripped, when `attr` is dotted
/// (`nvtopPackages.amd` — the head segment isn't a program name), or when
/// stripping would leave fewer than 2 characters. The caller falls back to
/// the raw attribute.
pub fn icon_base_name(attr: &str) -> Option<String> {
    if attr.contains('.') {
        return None;
    }
    let mut base = attr;
    while let Some(idx) = base.rfind(['-', '_']) {
        let token = &base[idx + 1..];
        if !ICON_BASE_SUFFIXES.contains(&token) {
            break;
        }
        let candidate = &base[..idx];
        if candidate.len() < 2 {
            break;
        }
        base = candidate;
    }
    (base != attr).then(|| base.to_string())
}

/// The Modulix module row(s) targeting `app_id`, best-effort and bounded by
/// `timeout` (timeout/error → no row, never an error to the caller). Does not
/// call `resolve()`: the popover row only needs the packaging format, not an
/// icon or description.
pub async fn module_rows_for_app_id(app_id: &str, timeout: std::time::Duration) -> Vec<AppEntry> {
    let modules = tokio::time::timeout(timeout, module_info::modules_for_app_id(app_id))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();

    // Every module here targets the same `app_id` (that's how they were
    // filtered), so its Flatpak/AppStream display name is the same for all.
    let flatpak_name = app_name_for_app_id(app_id);

    modules
        .iter()
        .map(|module| AppEntry {
            name: module.package_name().to_string(),
            base_name: module.package_name().to_string(),
            pname: flatpak_name
                .map(str::to_string)
                .unwrap_or_else(|| module.display_name().to_string()),
            app_name: None,
            summary: module.summary().to_string(),
            version: String::new(),
            app_id: Some(app_id.to_string()),
            group_id: Some(app_id.to_string()),
            icon: None,
            icon_name: module.icon_name().map(str::to_string),
            kind: "module",
            flatpak_preferred: false,
            variant_rank: 0,
            score: 0,
        })
        .collect()
}

/// Client-visible ordering within [`crate::store::Store::packages_for_app_id`]:
/// modules first, then packages by [`variant_rank`], ties broken by `name`.
pub fn alt_sort_key(e: &AppEntry) -> (u8, i32, &str) {
    (
        if e.kind == "module" { 0 } else { 1 },
        e.variant_rank,
        &e.name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_entry(name: &str, app_id: Option<&str>) -> AppEntry {
        AppEntry {
            name: name.to_string(),
            base_name: name.to_string(),
            pname: name.to_string(),
            app_name: None,
            summary: "summary".to_string(),
            version: "1.0".to_string(),
            group_id: app_id
                .map(str::to_string)
                .or_else(|| Some(name.to_string())),
            app_id: app_id.map(str::to_string),
            icon: None,
            icon_name: None,
            kind: "package",
            flatpak_preferred: false,
            variant_rank: 20,
            score: 0,
        }
    }

    fn grp_entry(name: &str, app_id: Option<&str>, group: &str) -> AppEntry {
        AppEntry {
            group_id: Some(group.to_string()),
            ..pkg_entry(name, app_id)
        }
    }

    #[test]
    fn dedup_by_group_collapses_same_pname() {
        let out = dedup_by_group(vec![
            grp_entry("nvtopPackages.full", None, "nvtop"),
            grp_entry("nvtopPackages.amd", None, "nvtop"),
            grp_entry("nvtopPackages.nvidia", None, "nvtop"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "nvtopPackages.full"); // first wins
    }

    #[test]
    fn dedup_by_group_keeps_distinct_pnames() {
        let out = dedup_by_group(vec![grp_entry("a", None, "a"), grp_entry("b", None, "b")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn dedup_by_group_collapses_same_app_id() {
        let out = dedup_by_group(vec![
            grp_entry(
                "firefox",
                Some("org.mozilla.firefox"),
                "org.mozilla.firefox",
            ),
            grp_entry(
                "firefox-bin",
                Some("org.mozilla.firefox"),
                "org.mozilla.firefox",
            ),
        ]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn variant_suffix_cases() {
        assert_eq!(
            variant_suffix("nvtopPackages.amd", "nvtop").as_deref(),
            Some("amd")
        );
        assert_eq!(variant_suffix("nvtop-amd", "nvtop").as_deref(), Some("amd"));
        assert_eq!(variant_suffix("nvtop", "nvtop"), None);
        assert_eq!(
            variant_suffix("firefox-esr", "firefox").as_deref(),
            Some("esr")
        );
    }

    #[test]
    fn variant_label_formats() {
        assert_eq!(variant_label("nvtop", "nvtopPackages.amd"), "nvtop (amd)");
        assert_eq!(variant_label("nvtop", "nvtop"), "nvtop");
    }

    #[test]
    fn base_attr_picks_shortest() {
        assert_eq!(
            base_attr(&["firefox-bin", "firefox", "firefox-esr"]),
            "firefox"
        );
    }

    #[test]
    fn base_attr_breaks_ties_lexicographically() {
        assert_eq!(base_attr(&["bbb", "aaa"]), "aaa");
    }

    #[test]
    fn variant_rank_known_suffixes() {
        assert_eq!(variant_rank("firefox", "firefox"), 20);
        assert_eq!(variant_rank("firefox-bin", "firefox"), 10);
        assert_eq!(variant_rank("firefox-esr", "firefox"), 100);
        assert_eq!(variant_rank("firefox-esr-bin", "firefox"), 90);
        assert_eq!(variant_rank("firefox-unwrapped", "firefox"), 800);
        assert_eq!(variant_rank("firefox-nonsense", "firefox"), 900);
    }

    #[test]
    fn icon_base_name_strips_known_suffixes() {
        assert_eq!(
            icon_base_name("davinci-resolve-studio").as_deref(),
            Some("davinci-resolve")
        );
        assert_eq!(
            icon_base_name("firefox-esr-unwrapped").as_deref(),
            Some("firefox")
        );
        assert_eq!(icon_base_name("nvtopPackages.amd"), None);
        assert_eq!(icon_base_name("code"), None);
        assert_eq!(icon_base_name("vscode-fhs").as_deref(), Some("vscode"));
    }
}
