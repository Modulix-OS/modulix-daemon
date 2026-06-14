use super::*;

#[test]
fn install_package_name_matches_method() {
    assert_eq!(InstallPackage.name(), "InstallPackage");
}

#[test]
fn uninstall_package_name_matches_method() {
    assert_eq!(UninstallPackage.name(), "UninstallPackage");
}

#[tokio::test]
async fn install_package_reports_success() {
    let result = InstallPackage.execute(&["htop"]).await.unwrap();
    assert_eq!(result, "package htop installed");
}

#[tokio::test]
async fn uninstall_package_reports_success() {
    let result = UninstallPackage.execute(&["htop"]).await.unwrap();
    assert_eq!(result, "package htop uninstalled");
}
