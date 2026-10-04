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
async fn update_system_switch_is_a_deferred_apply() {
    // `"switch"` is kept only as a synonym of `"boot"`: nothing switches the
    // running system, so it must answer with the next-boot message.
    let result = UpdateSystem.execute(&["switch"]).await.unwrap();
    assert_eq!(result, "system update prepared for next boot");
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

#[tokio::test]
async fn update_system_build_reports_success() {
    let result = UpdateSystem.execute(&["build"]).await.unwrap();
    assert_eq!(result, "system update downloaded");
}

#[tokio::test]
async fn update_system_stage_reports_success() {
    let result = UpdateSystem.execute(&["stage"]).await.unwrap();
    assert_eq!(result, "system update downloaded");
}

#[tokio::test]
async fn update_system_apply_reports_success() {
    let result = UpdateSystem.execute(&["apply"]).await.unwrap();
    assert_eq!(result, "system update prepared for next boot");
}

#[test]
fn background_cores_is_half_and_at_least_one() {
    let cores = background_cores().expect("capped");
    assert!(cores >= 1);
    assert_eq!(cores, half_cores(available_cores()));
}

#[test]
fn success_message_per_mode() {
    assert_eq!(
        success_message("switch"),
        "system update prepared for next boot"
    );
    assert_eq!(success_message("build"), "system update downloaded");
    assert_eq!(success_message("stage"), "system update downloaded");
    assert_eq!(
        success_message("boot"),
        "system update prepared for next boot"
    );
    assert_eq!(
        success_message("apply"),
        "system update prepared for next boot"
    );
}
