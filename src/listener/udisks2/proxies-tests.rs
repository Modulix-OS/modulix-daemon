use zbus::proxy::Defaults;

use super::*;

/// The generated proxy's defaults are what every `BlockProxy::builder` call in
/// this module relies on: they only pass a `path`, never a destination or an
/// interface.
#[test]
fn block_proxy_defaults_target_udisks2() {
    assert_eq!(
        <BlockProxy as Defaults>::INTERFACE
            .as_ref()
            .map(|i| i.as_str()),
        Some("org.freedesktop.UDisks2.Block")
    );
    assert_eq!(
        <BlockProxy as Defaults>::DESTINATION
            .as_ref()
            .map(|d| d.to_string())
            .as_deref(),
        Some("org.freedesktop.UDisks2")
    );
}

/// There is no single object path to default to: one `Block` object exists per
/// device, so the caller must always supply it.
#[test]
fn block_proxy_has_no_default_path() {
    assert!(<BlockProxy as Defaults>::PATH.is_none());
}
