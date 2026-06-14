use super::*;

#[test]
fn install_module_name_matches_method() {
    assert_eq!(InstallModule.name(), "InstallModule");
}

#[test]
fn uninstall_module_name_matches_method() {
    assert_eq!(UninstallModule.name(), "UninstallModule");
}

#[tokio::test]
async fn install_module_reports_success() {
    let result = InstallModule.execute(&["audio"]).await.unwrap();
    assert_eq!(result, "module audio installed");
}

#[tokio::test]
async fn uninstall_module_reports_success() {
    let result = UninstallModule.execute(&["audio"]).await.unwrap();
    assert_eq!(result, "module audio uninstalled");
}
