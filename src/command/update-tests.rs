use super::*;

#[test]
fn update_system_name_matches_method() {
    assert_eq!(UpdateSystem.name(), "UpdateSystem");
}

#[test]
fn half_cores_rounds_down() {
    assert_eq!(half_cores(8), 4);
    assert_eq!(half_cores(5), 2);
}

#[test]
fn half_cores_never_below_one() {
    assert_eq!(half_cores(1), 1);
    assert_eq!(half_cores(0), 1);
}

#[tokio::test]
async fn update_system_switch_reports_success() {
    let result = UpdateSystem.execute(&["switch"]).await.unwrap();
    assert_eq!(result, "system updated (switch)");
}

#[tokio::test]
async fn update_system_boot_reports_success() {
    let result = UpdateSystem.execute(&["boot"]).await.unwrap();
    assert_eq!(result, "system update prepared for next boot");
}

#[tokio::test]
async fn update_system_missing_mode_errors() {
    let result = UpdateSystem.execute(&[]).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn update_system_unknown_mode_errors() {
    let result = UpdateSystem.execute(&["frobnicate"]).await;
    assert!(result.is_err());
}
