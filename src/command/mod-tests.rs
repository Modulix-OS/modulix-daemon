use super::*;

#[test]
fn registry_contains_package_commands() {
    let names: Vec<_> = registry().iter().map(|c| c.name()).collect();
    assert_eq!(
        names,
        vec![
            "InstallPackage",
            "UninstallPackage",
            "InstallModule",
            "UninstallModule",
            "InstallPlugin",
            "UninstallPlugin",
        ]
    );
}
