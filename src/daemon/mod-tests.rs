use super::*;

#[test]
fn bus_name_matches_deployed_name() {
    assert_eq!(BUS_NAME, "org.modulix.Daemon");
}

#[test]
fn object_path_is_well_formed() {
    assert!(OBJECT_PATH.starts_with('/'));
}

#[tokio::test]
async fn install_package_delegates_to_command() {
    let daemon = Daemon::new();
    let result = daemon.install_package("htop").await.unwrap();
    assert_eq!(result, "package htop installed");
}

#[tokio::test]
async fn uninstall_package_delegates_to_command() {
    let daemon = Daemon::new();
    let result = daemon.uninstall_package("htop").await.unwrap();
    assert_eq!(result, "package htop uninstalled");
}

#[tokio::test]
async fn install_module_delegates_to_command() {
    let daemon = Daemon::new();
    let result = daemon.install_module("audio").await.unwrap();
    assert_eq!(result, "module audio installed");
}

#[tokio::test]
async fn uninstall_module_delegates_to_command() {
    let daemon = Daemon::new();
    let result = daemon.uninstall_module("audio").await.unwrap();
    assert_eq!(result, "module audio uninstalled");
}

#[tokio::test]
async fn install_plugin_delegates_to_command() {
    let daemon = Daemon::new();
    let result = daemon.install_plugin("audio", "reverb").await.unwrap();
    assert_eq!(result, "plugin reverb installed for module audio");
}

#[tokio::test]
async fn uninstall_plugin_delegates_to_command() {
    let daemon = Daemon::new();
    let result = daemon.uninstall_plugin("audio", "reverb").await.unwrap();
    assert_eq!(result, "plugin reverb uninstalled for module audio");
}
