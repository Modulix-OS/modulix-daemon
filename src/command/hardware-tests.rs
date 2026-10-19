use super::*;

#[test]
fn update_hardware_name_matches_method() {
    assert_eq!(UpdateHardware.name(), "UpdateHardware");
}

#[tokio::test]
async fn update_hardware_reports_success() {
    let result = UpdateHardware.execute(&[]).await.unwrap();
    assert_eq!(result, "hardware configuration updated");
}

#[tokio::test]
async fn update_hardware_ignores_arguments() {
    let result = UpdateHardware.execute(&["unexpected"]).await.unwrap();
    assert_eq!(result, "hardware configuration updated");
}
