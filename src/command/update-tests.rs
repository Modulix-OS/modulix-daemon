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

#[test]
fn background_cores_is_half_and_at_least_one() {
    let cores = background_cores().expect("capped");
    assert!(cores >= 1);
    assert_eq!(cores, half_cores(available_cores()));
}

/// The whole mode table in one place: `"switch"` replaces the running system,
/// `"boot"` only prepares the next boot, and nothing else is a mode.
/// `BuildCommand` derives neither `Debug` nor `PartialEq`, hence `matches!`;
/// and its `as_str` is deliberately not used here, since a debug build
/// substitutes `build-vm` for both variants.
#[test]
fn build_command_per_mode() {
    assert!(matches!(
        build_command("switch"),
        Some(BuildCommand::Switch)
    ));
    assert!(matches!(build_command("boot"), Some(BuildCommand::Boot)));
}

/// `build_command` is the mode validation, so an unknown mode must be the only
/// thing it rejects - including the ones a client might plausibly invent, and
/// the three modes this command used to have.
#[test]
fn build_command_rejects_unknown_modes() {
    for mode in [
        "",
        "frobnicate",
        "Switch",
        "boot ",
        "build",
        "stage",
        "apply",
        "build-vm",
        "install",
    ] {
        assert!(build_command(mode).is_none(), "{mode:?}");
    }
}

#[test]
fn success_message_per_mode() {
    assert_eq!(success_message("switch"), "system updated");
    assert_eq!(
        success_message("boot"),
        "system update prepared for next boot"
    );
}

#[tokio::test]
async fn update_system_switch_reports_an_applied_update() {
    // `"switch"` is the only mode that activates the running system, so its
    // reply must not be the next-boot one.
    let result = UpdateSystem.execute(&["switch"]).await.unwrap();
    assert_eq!(result, "system updated");
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

/// An invalid mode must be refused before any work starts, and with the exact
/// message clients get - `Error::CoreUtils`'s payload is what reaches the
/// wire, verbatim, as the `Failed` reply's message.
#[tokio::test]
async fn update_system_unknown_mode_message_is_verbatim() {
    let err = UpdateSystem.execute(&["frobnicate"]).await.unwrap_err();
    let Error::CoreUtils(message) = err else {
        panic!("expected Error::CoreUtils, got {err:?}");
    };
    assert_eq!(
        message,
        "UpdateSystem: unknown mode 'frobnicate', expected 'switch' or 'boot'"
    );
}

/// The modes that disappeared with the staging area must be refused, not
/// silently treated as one of the two that remain: a client still sending
/// `"build"` has to find out.
#[tokio::test]
async fn update_system_rejects_the_removed_modes() {
    for mode in ["build", "stage", "apply"] {
        assert!(UpdateSystem.execute(&[mode]).await.is_err(), "{mode}");
    }
}
