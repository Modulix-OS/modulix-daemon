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

/// One `org.modulix.Store1` row as sent over D-Bus: an `a{sv}` dict, string
/// key to variant value. Built by [`AppEntry::into_dict`],
/// [`PluginEntry::into_dict`] or [`EnrichEntry::into_dict`]; consumed
/// client-side by `modulix-store-client/src/convert.rs`'s `dict_to_json`,
/// which the C plugin then reads through `gs_modulix_json_*`
/// (`gnome-software-plugin/plugin/src/gs-modulix-app.c`,
/// `gs-modulix-refine.c`).
pub type Dict = HashMap<String, OwnedValue>;

/// Wraps `v` as an [`OwnedValue`]. Infallible for every type used in this
/// module (only `zvariant::Value::Fd` conversion can fail).
///
/// # Parameters
/// * `v` - the value to wrap; any type with a `Value<'static>: From<T>` impl
///   (the scalar and tuple/vec types this module emits).
///
/// # Returns
/// `v` re-encoded as an [`OwnedValue`], ready to insert into a [`Dict`].
///
/// # Panics
/// Never, for the types actually passed to it in this module. Would panic
/// via `.expect(..)` only for a `zvariant::Value::Fd`, which this module
/// never constructs.
fn ov<T>(v: T) -> OwnedValue
where
    Value<'static>: From<T>,
{
    OwnedValue::try_from(Value::from(v)).expect("infallible for non-fd values")
}

/// One installable app row (nix package or Modulix module), store-side and
/// bus-neutral. Serialized to a D-Bus `a{sv}` dict by [`Self::into_dict`];
/// read back client-side by `gs_modulix_make_app_from_json`
/// (`gnome-software-plugin/plugin/src/gs-modulix-app.c`).
#[derive(Clone, Debug)]
pub struct AppEntry {
    /// nixpkgs attribute (package) or module name — the install identifier.
    /// Bus key `"name"`, zvariant `s`. Always present. Required by the C
    /// consumer: `gs_modulix_make_app_from_json` returns `NULL` (drops the
    /// row) when this is missing or empty.
    pub name: String,
    /// Themed-icon fallback: `name` with any known variant/edition suffix
    /// stripped, via [`icon_base_name`]; falls back to `name` unchanged.
    /// Bus key `"base_name"`, zvariant `s`. Always present (client also
    /// falls back to `name` if it were ever empty/absent).
    pub base_name: String,
    /// Human display name (pname / module display name). Bus key `"pname"`,
    /// zvariant `s`. Always present.
    pub pname: String,
    /// Flatpak/AppStream display name, when the package matches a known app.
    /// Bus key `"app_name"`, zvariant `s`. Absent from the dict when `None`
    /// (see [`Self::into_dict`]); read by the C consumer but not required.
    pub app_name: Option<String>,
    /// One-line package/app summary. Bus key `"summary"`, zvariant `s`.
    /// Always present (empty string when the source has none).
    pub summary: String,
    /// Package version string. Bus key `"version"`, zvariant `s`. Always
    /// present; empty for module entries, which carry no nix version.
    pub version: String,
    /// Canonical AppStream id used to deduplicate against Flatpak/AppStream.
    /// Bus key `"app_id"`, zvariant `s`. Absent from the dict when `None`.
    pub app_id: Option<String>,
    /// Stable grouping key: `app_id` when known, otherwise `pname`. Bus key
    /// `"group_id"`, zvariant `s`. Absent from the dict when `None`. Read by
    /// the C consumer as the `GsApp` id (`app_unique_id`), falling back to
    /// `app_id` then `name` when absent.
    pub group_id: Option<String>,
    /// Icon URL or path. Bus key `"icon"`, zvariant `s`. Absent from the
    /// dict when `None`.
    pub icon: Option<String>,
    /// Themed icon name (`meta.mainProgram`). Bus key `"icon_name"`,
    /// zvariant `s`. Absent from the dict when `None`.
    pub icon_name: Option<String>,
    /// Row kind discriminator: `"package"` or `"module"`. Bus key `"kind"`,
    /// zvariant `s` (the `&'static str` is re-encoded as an owned `String`
    /// in [`Self::into_dict`]). Always present. The C consumer compares it
    /// against `"module"` to pick the module-vs-package code path.
    pub kind: &'static str,
    /// Whether the Flatpak/AppStream counterpart of this app should be
    /// preferred over this nix entry. Bus key `"flatpak_preferred"`,
    /// zvariant `b`. Always present.
    pub flatpak_preferred: bool,
    /// Ordering rank of this entry within its variant group (see
    /// [`variant_rank`]). Meaningless across groups; combine with `kind` on
    /// the client to reproduce the Sources-popover order. Bus key
    /// `"variant_rank"`, zvariant `i`. Always present.
    pub variant_rank: i32,
    /// Raw search relevance (`0` outside search paths). Bus key `"score"`,
    /// zvariant `u`. Absent from the dict when `0` (see
    /// [`Self::into_dict`]); the C consumer treats an absent key as `0`
    /// (`has_score` check), so this omission is lossless.
    pub score: u32,
    /// Whether this exact `name` is currently in the system configuration.
    /// Stamped by `crate::store` *after* every cache read (see
    /// `stamp_installed`), never inside a cached value: a search result held
    /// for 60s must not carry a 60s-old install state. Bus key
    /// `"installed"`, zvariant `b`. Always present (unlike `score`) so a
    /// client can distinguish "not installed" from "daemon too old to say";
    /// the C consumer falls back to its own `is_installed` argument only
    /// when this key is absent, i.e. only against a pre-this-field daemon.
    pub installed: bool,
}

impl AppEntry {
    /// Serializes `self` into the `a{sv}` [`Dict`] sent over
    /// `org.modulix.Store1`, one bus key per field as documented on each
    /// field of [`AppEntry`] above.
    ///
    /// # Parameters
    /// None beyond `self`, consumed by value.
    ///
    /// # Returns
    /// A [`Dict`] with `"name"`, `"base_name"`, `"pname"`, `"summary"`,
    /// `"version"`, `"kind"`, `"flatpak_preferred"`, `"variant_rank"` and
    /// `"installed"` always present; `"app_name"`, `"app_id"`, `"group_id"`,
    /// `"icon"` and `"icon_name"` present only when the corresponding
    /// `Option` is `Some`; `"score"` present only when `self.score > 0`.
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
        d.insert("installed".into(), ov(self.installed));
        d
    }
}

/// One Modulix module-plugin (addon) row, store-side and bus-neutral.
/// Serialized to a D-Bus `a{sv}` dict by [`Self::into_dict`]; read back
/// client-side as a `GsApp` addon by
/// `gs_modulix_add_module_plugins`/`gs-modulix-plugins-cache.h`
/// (`gnome-software-plugin/plugin/src/gs-modulix-app.c`).
#[derive(Clone)]
pub struct PluginEntry {
    /// Plugin identifier — the install identifier for this addon. Bus key
    /// `"name"`, zvariant `s`. Always present.
    pub name: String,
    /// Human-readable plugin description. Bus key `"description"`,
    /// zvariant `s`. Always present.
    pub description: String,
    /// Same rationale as `AppEntry::installed`: always emitted, so a client
    /// can tell "not installed" from "daemon too old to say". Stamped after
    /// the `PLUGINS_CACHE` read (see `store::stamp_plugins_installed`), never
    /// baked into the cached value — that cache lives 5 minutes, longer than
    /// an install takes. Bus key `"installed"`, zvariant `b`. Always
    /// present; the C consumer falls back to a default only when the key is
    /// absent (pre-this-field daemon).
    pub installed: bool,
}

impl PluginEntry {
    /// Serializes `self` into the `a{sv}` [`Dict`] sent over
    /// `org.modulix.Store1` for a module's plugin list.
    ///
    /// # Parameters
    /// None beyond `self`, consumed by value.
    ///
    /// # Returns
    /// A [`Dict`] with `"name"`, `"description"` and `"installed"` always
    /// present (no field of `PluginEntry` is optional).
    pub fn into_dict(self) -> Dict {
        let mut d = Dict::new();
        d.insert("name".into(), ov(self.name));
        d.insert("description".into(), ov(self.description));
        d.insert("installed".into(), ov(self.installed));
        d
    }
}

/// One screenshot row, the element type of `EnrichEntry::screenshots` and
/// thus of the bus value under key `"screenshots"`: a 3-tuple
/// `(caption, is_default, images)`; `images` is `(url, width, height)`
/// tuples. D-Bus signature `(sba(suu))` per element, so the `Vec<ShotTuple>`
/// as a whole has signature `a(sba(suu))`.
///
/// # Fields
/// * `.0` `caption` - screenshot caption; zvariant `s`. Set to `""` by
///   [`collect_screenshots`] when the source screenshot has none. Read
///   client-side as JSON `"caption"` by `shots_to_json`
///   (`modulix-store-client/src/convert.rs`) then consumed by
///   `gs_modulix_add_app_screenshots` (`gnome-software-plugin/plugin/src/gs-modulix-app.c`),
///   which falls back to no caption when absent/empty.
/// * `.1` `is_default` - whether this is the AppStream default screenshot
///   (picked as `AS_SCREENSHOT_KIND_DEFAULT` vs `_EXTRA` client-side);
///   zvariant `b`. Read client-side as JSON `"default"`.
/// * `.2` `images` - the sized variants of this screenshot, each a 3-tuple
///   `(url, width, height)`: `url` (zvariant `s`) the image URL — an image
///   whose URL cannot be read as a string is dropped client-side; `width`
///   and `height` (zvariant `u` each) its pixel dimensions, defaulting to
///   `0` client-side when missing/not a `u32`. Read client-side as JSON
///   `"images": [{url, width, height}]`.
pub type ShotTuple = (String, bool, Vec<(String, u32, u32)>);

/// Store-fetched app enrichment (Flathub/AppStream metadata not available
/// from nixpkgs alone), store-side and bus-neutral. Serialized to a D-Bus
/// `a{sv}` dict by [`Self::into_dict`]; read back client-side by
/// `gs_modulix_add_app_screenshots` (screenshots) and the refine path in
/// `gnome-software-plugin/plugin/src/gs-modulix-refine.c` (description,
/// license).
#[derive(Clone)]
pub struct EnrichEntry {
    /// HTML app description (AppStream `<description>`). Bus key
    /// `"description"`, zvariant `s`. Absent from the dict when `None`.
    /// Read client-side (`gs-modulix-refine.c`) as the app's formatted
    /// description.
    pub description: Option<String>,
    /// Screenshot list; see [`ShotTuple`] for the per-element wire shape.
    /// Bus key `"screenshots"`, zvariant `a(sba(suu))`. Absent from the
    /// dict when empty (see [`Self::into_dict`]); the client tolerates a
    /// missing or non-array value by treating it as no screenshots
    /// (`gs_modulix_add_app_screenshots`).
    pub screenshots: Vec<ShotTuple>,
    /// Flathub AppStream icon URL. Bus key `"icon"`, zvariant `s`. Absent
    /// from the dict when `None`.
    pub icon: Option<String>,
    /// Themed icon name from the enrichment source. Bus key `"icon_name"`,
    /// zvariant `s`. Absent from the dict when `None`.
    pub icon_name: Option<String>,
    /// SPDX expression (or an AppStream `LicenseRef-*`) for the app. Bus
    /// key `"license"`, zvariant `s`. Absent from the dict when `None`.
    /// Read client-side (`gs-modulix-refine.c`) via `gs_app_set_license`.
    pub license: Option<String>,
}

impl EnrichEntry {
    /// Serializes `self` into the `a{sv}` [`Dict`] sent over
    /// `org.modulix.Store1` for an enrichment row.
    ///
    /// # Parameters
    /// None beyond `self`, consumed by value.
    ///
    /// # Returns
    /// A [`Dict`] with `"description"`, `"icon"`, `"icon_name"` and
    /// `"license"` present only when the corresponding `Option` is `Some`;
    /// `"screenshots"` present only when `self.screenshots` is non-empty.
    /// The dict is `{}` when every field is `None`/empty.
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

/// Converts the crate's borrowed [`AppScreenshot`] into owned `(sba(suu))`
/// tuples ready for [`EnrichEntry::screenshots`]/the bus.
///
/// # Parameters
/// * `shot` - the source app's screenshot set, or `None` when it has none.
///
/// # Returns
/// One [`ShotTuple`] per screenshot in `shot.screenshots`, in source order;
/// `caption` and each image's `url`/`width`/`height` are copied verbatim,
/// `is_default` is `true` exactly for the element at index `shot.default`.
/// An empty `Vec` when `shot` is `None`.
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

/// Builds an [`AppEntry`] (`kind: "package"`) from a resolved nix package.
///
/// # Parameters
/// * `pkg` - the resolved nix package to describe.
/// * `score` - raw search relevance to stamp onto the entry's `score` field
///   (`0` outside search paths).
///
/// # Returns
/// An [`AppEntry`] with `name`/`pname`/`summary`/`version`/`app_name`/
/// `icon`/`icon_name` taken from `pkg`; `app_id` from `pkg.id()`;
/// `flatpak_preferred` from [`package_info::is_flatpak_preferred`] on that
/// id (`false` when `pkg` has no id); `group_id` = `app_id`, else `pname`
/// when non-empty, else `name`; `variant_rank` from [`variant_rank`];
/// `base_name` from [`icon_base_name`], falling back to `name`; `kind`
/// `"package"`; `installed` always `false` (stamped later by the caller,
/// see `AppEntry::installed`). Grouping by app-id when known
/// (firefox/firefox-bin), else by pname, is what lets same-pname variants
/// (nvtopPackages.amd/…) collapse to one row.
pub fn package_entry(pkg: &NixPackage, score: u32) -> AppEntry {
    let app_id = pkg.id().map(str::to_string);
    let flatpak_preferred = app_id
        .as_deref()
        .map(package_info::is_flatpak_preferred)
        .unwrap_or(false);
    let name = pkg.package_name().to_string();
    let pname = pkg.display_name().to_string();
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
        installed: false,
    }
}

/// Flatpak/AppStream display name for the app a Modulix module targets, via
/// the curated table also used for nix packages.
///
/// # Parameters
/// * `app_id` - the AppStream id to look up.
///
/// # Returns
/// The display name of the first nix package known to target `app_id`
/// (via [`package_info::packages_for_app_id`] then
/// [`package_info::name_for_package`]); `None` when `app_id` has no known
/// package or that package has no name in the table.
pub fn app_name_for_app_id(app_id: &str) -> Option<&'static str> {
    package_info::packages_for_app_id(app_id)
        .first()
        .and_then(|attr| package_info::name_for_package(attr))
}

/// Same table-lookup trick as [`app_name_for_app_id`], for the themed icon name.
///
/// # Parameters
/// * `app_id` - the AppStream id to look up.
///
/// # Returns
/// The themed icon name of the first nix package known to target `app_id`;
/// `None` when `app_id` has no known package or that package has no icon
/// name in the table.
pub fn icon_name_for_app_id(app_id: &str) -> Option<&'static str> {
    package_info::packages_for_app_id(app_id)
        .first()
        .and_then(|attr| package_info::icon_name_for_package(attr))
}

/// Builds an [`AppEntry`] (`kind: "module"`) from a Modulix module.
///
/// # Parameters
/// * `module` - the module to describe. Call after `resolve()` so
///   `module.icon()` is populated.
/// * `score` - raw search relevance to stamp onto the entry's `score` field
///   (`0` outside search paths).
///
/// # Returns
/// An [`AppEntry`] with `name`/`base_name` = `module.package_name()`;
/// `pname` = the Flatpak/AppStream name for `module.id()` when known (via
/// [`app_name_for_app_id`]), else `module.display_name()`; `app_id` =
/// `module.id()`; `group_id` = `app_id` (modules never group by pname);
/// `version` empty; `icon`/`icon_name` from `module`; `kind` `"module"`;
/// `flatpak_preferred` always `false`; `variant_rank` always `0`;
/// `installed` always `false` (stamped later by the caller).
///
/// # Pre-conditions
/// `module.resolve()` must have run beforehand, or `module.icon()` will be
/// unpopulated and the entry's `icon` field will be `None`.
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
        group_id: app_id.clone(),
        app_id,
        icon: module.icon().map(str::to_string),
        icon_name: module.icon_name().map(str::to_string),
        kind: "module",
        flatpak_preferred: false,
        variant_rank: 0,
        score,
        installed: false,
    }
}

/// Collapse nix variants that share a grouping key into one entry: same-app-id
/// variants (`firefox`, `firefox-bin` → one "Firefox") **and** same-pname
/// variants (`nvtopPackages.amd`, `nvtopPackages.nvidia` → one "nvtop").
/// Entries without a `group_id` are kept individually. The dropped variants
/// stay reachable through [`crate::store::Store::packages_for_app_id`].
///
/// An **installed** variant represents its group; otherwise the first entry
/// per key wins, search results being relevance-sorted. That exception is what
/// keeps the row the store displays the one the user actually has — and the
/// one its Uninstall button targets: with `firefox-bin` installed but
/// `firefox` scoring higher, first-wins would show an available `firefox`.
/// Callers must therefore stamp `installed` *before* deduplicating (see
/// `crate::store::stamp_installed`).
///
/// # Parameters
/// * `entries` - the entries to deduplicate, consumed by value.
///
/// # Pre-conditions
/// Each entry's `installed` field must already reflect the current system
/// state (see `crate::store::stamp_installed`); this function does not
/// stamp it.
///
/// # Returns
/// One entry per distinct `group_id`, plus every entry with `group_id ==
/// None` kept individually, in the input order of first appearance. Within
/// a group, the first installed entry wins if any is installed, otherwise
/// the first entry in input order. Internally, a slot in `winners` is
/// overwritten in place rather than duplicated, so every index in
/// `winners` stays distinct and each entry can be `take()`n out of
/// `entries` exactly once while still in place.
pub fn dedup_by_group(entries: Vec<AppEntry>) -> Vec<AppEntry> {
    let mut winners: Vec<usize> = Vec::new();
    let mut slot_of: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for (i, e) in entries.iter().enumerate() {
        let Some(gid) = &e.group_id else {
            winners.push(i);
            continue;
        };
        match slot_of.get(gid) {
            None => {
                slot_of.insert(gid.clone(), winners.len());
                winners.push(i);
            }
            Some(&slot) if e.installed && !entries[winners[slot]].installed => {
                winners[slot] = i;
            }
            Some(_) => {}
        }
    }

    let mut entries: Vec<Option<AppEntry>> = entries.into_iter().map(Some).collect();
    winners
        .into_iter()
        .filter_map(|i| entries[i].take())
        .collect()
}

/// Distinguishing suffix of a nix attribute within its pname group, used to
/// label stacked variants: `nvtopPackages.amd` → `amd`, `nvtop-amd` → `amd`,
/// `firefox-esr` → `esr`. `None` when the attribute carries no extra suffix
/// over the bare pname (e.g. `nvtop`/`nvtop`).
///
/// # Parameters
/// * `attr` - the nix attribute to inspect (e.g. `nvtopPackages.amd`).
/// * `pname` - the group's bare pname (e.g. `nvtop`), used to strip a
///   leading-pname prefix when `attr` has no dotted segment.
///
/// # Returns
/// `Some(suffix)`: the text after the last `.` when `attr` is dotted; else
/// the text after `pname` (with a leading `-`/`_` trimmed) when `attr`
/// starts with `pname`; else `attr` itself when it differs from `pname`.
/// `None` when `attr == pname`, or when the dotted/prefixed remainder is
/// empty.
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
///
/// # Parameters
/// * `pname` - the group's bare pname.
/// * `attr` - the nix attribute whose suffix (see [`variant_suffix`])
///   distinguishes it within the group.
///
/// # Returns
/// `"{pname} ({suffix})"` when [`variant_suffix`] returns `Some`, else the
/// bare `pname`.
pub fn variant_label(pname: &str, attr: &str) -> String {
    match variant_suffix(attr, pname) {
        Some(suffix) => format!("{pname} ({suffix})"),
        None => pname.to_string(),
    }
}

/// The "base" nix attribute of a group of variants sharing one app-id/pname:
/// the shortest attribute, lexicographic order breaking ties.
///
/// # Parameters
/// * `attrs` - the group's nix attributes.
///
/// # Returns
/// The shortest string in `attrs`, lexicographically smallest among
/// equal-length candidates; `""` when `attrs` is empty.
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
///
/// This is the value stored on the bus as `AppEntry`'s `"variant_rank"` key
/// (see [`AppEntry::variant_rank`]).
///
/// # Parameters
/// * `attr` - the nix attribute to rank.
/// * `base` - the group's bare pname/base attribute, passed to
///   [`variant_suffix`] to compute `attr`'s suffix.
///
/// # Returns
/// `20` for the base attribute (no suffix); `10` for `-bin`; `30`/`40` for
/// `-beta-bin`/`-beta`; `50`/`60` for `-nightly-bin`/`-nightly`; `70`/`80`
/// for `-devedition-bin`/`-devedition`; `90`/`100` for `-esr-bin`/`-esr`;
/// `800` for `-unwrapped`; `900` for any other suffix.
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
    "static",
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

/// Strips trailing `-<suffix>`/`_<suffix>` tokens in [`ICON_BASE_SUFFIXES`]
/// from a nix attribute, e.g. `davinci-resolve-studio` → `davinci-resolve`.
/// Repeats until no further known suffix is found, so multiple trailing
/// suffixes are all stripped (e.g. `firefox-esr-unwrapped` → `firefox`).
///
/// This is the value stored on the bus as `AppEntry`'s `"base_name"` key
/// when `Some` (see [`AppEntry::base_name`]); callers fall back to the raw
/// attribute when `None`.
///
/// # Parameters
/// * `attr` - the nix attribute to strip suffixes from.
///
/// # Returns
/// `Some(base)` with every trailing known suffix removed. `None` when no
/// suffix was stripped, when `attr` is dotted (`nvtopPackages.amd` — the
/// head segment isn't a program name), or when stripping would leave fewer
/// than 2 characters.
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
///
/// # Parameters
/// * `app_id` - the AppStream id to find module rows for.
/// * `timeout` - upper bound on how long to wait for
///   [`module_info::modules_for_app_id`].
///
/// # Returns
/// One [`AppEntry`] (`kind: "module"`) per module targeting `app_id`, each
/// with `pname` = the shared Flatpak/AppStream name for `app_id` when known
/// (via [`app_name_for_app_id`]) else the module's own display name — looked
/// up once for all modules, since every module here targets the same
/// `app_id` (that's how they were filtered) and so shares the same name;
/// `app_id`/`group_id` both set to `app_id`; `icon` always `None` (no
/// `resolve()` call); `version` empty; `score` `0`; `variant_rank` `0`;
/// `flatpak_preferred` `false`; `installed` `false`. An empty `Vec` on
/// timeout or lookup error — never propagated as an error to the caller.
///
/// # Errors
/// Never returns an error; a timeout or an `Err` from
/// [`module_info::modules_for_app_id`] both collapse to an empty result.
pub async fn module_rows_for_app_id(app_id: &str, timeout: std::time::Duration) -> Vec<AppEntry> {
    let modules = tokio::time::timeout(timeout, module_info::modules_for_app_id(app_id))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();

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
            installed: false,
        })
        .collect()
}

/// Client-visible ordering within [`crate::store::Store::packages_for_app_id`]:
/// modules first, then packages by [`variant_rank`], ties broken by `name`.
///
/// # Parameters
/// * `e` - the entry to compute a sort key for.
///
/// # Returns
/// A tuple sortable ascending: `0` for `e.kind == "module"` else `1`, then
/// `e.variant_rank`, then `&e.name` for a final lexicographic tie-break.
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

    /// Builds a minimal test [`AppEntry`] with fixed `summary`/`version`
    /// and `kind: "package"`.
    ///
    /// # Parameters
    /// * `name` - value for both `name` and `pname` (and `base_name`).
    /// * `app_id` - value for `app_id`; also used for `group_id` when
    ///   `Some`, else `group_id` falls back to `name`.
    ///
    /// # Returns
    /// The constructed [`AppEntry`], `installed: false`, `score: 0`,
    /// `variant_rank: 20`.
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
            installed: false,
        }
    }

    /// Like [`pkg_entry`], but with an explicit `group_id` override — used
    /// to test [`dedup_by_group`] independently of the `app_id`-vs-`pname`
    /// derivation `package_entry` normally performs.
    ///
    /// # Parameters
    /// * `name` - forwarded to [`pkg_entry`].
    /// * `app_id` - forwarded to [`pkg_entry`].
    /// * `group` - value for `group_id`, overriding [`pkg_entry`]'s default.
    ///
    /// # Returns
    /// The constructed [`AppEntry`], identical to [`pkg_entry`]'s output
    /// except for `group_id`.
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
        assert_eq!(out[0].name, "nvtopPackages.full");
    }

    #[test]
    fn dedup_by_group_installed_variant_wins() {
        let mut installed = grp_entry("nvtopPackages.amd", None, "nvtop");
        installed.installed = true;
        let out = dedup_by_group(vec![
            grp_entry("nvtopPackages.full", None, "nvtop"),
            installed,
            grp_entry("nvtopPackages.nvidia", None, "nvtop"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "nvtopPackages.amd");
        assert!(out[0].installed);
    }

    /// Replacing a group's winner must not move the group in the output.
    #[test]
    fn dedup_by_group_keeps_group_order() {
        let mut installed = grp_entry("b2", None, "b");
        installed.installed = true;
        let out = dedup_by_group(vec![
            grp_entry("a1", None, "a"),
            grp_entry("b1", None, "b"),
            installed,
            grp_entry("c1", None, "c"),
        ]);
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a1", "b2", "c1"]);
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
