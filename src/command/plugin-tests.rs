use super::*;

#[test]
fn install_plugin_name_matches_method() {
    assert_eq!(InstallPlugin.name(), "InstallPlugin");
}

#[test]
fn uninstall_plugin_name_matches_method() {
    assert_eq!(UninstallPlugin.name(), "UninstallPlugin");
}

#[tokio::test]
async fn install_plugin_reports_success() {
    let result = InstallPlugin.execute(&["audio", "reverb"]).await.unwrap();
    assert_eq!(result, "plugin reverb installed for module audio");
}

#[tokio::test]
async fn uninstall_plugin_reports_success() {
    let result = UninstallPlugin.execute(&["audio", "reverb"]).await.unwrap();
    assert_eq!(result, "plugin reverb uninstalled for module audio");
}
