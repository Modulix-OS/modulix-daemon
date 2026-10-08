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
    assert_eq!(success_message("switch"), "system updated");
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

/// The whole mode table in one place: `"build"`, `"stage"` and `"boot"` all
/// stage and activate nothing - promoting the staged system is the shutdown
/// unit's job - only `"switch"` activates the running system, and only
/// `"apply"` skips the staging step.
#[test]
fn steps_per_mode() {
    for mode in ["build", "stage", "boot"] {
        assert_eq!(
            steps(mode),
            Some(Steps {
                stage: true,
                activation: None
            }),
            "{mode}"
        );
    }
    assert_eq!(
        steps("switch"),
        Some(Steps {
            stage: true,
            activation: Some(Activation::Switch)
        })
    );
    assert_eq!(
        steps("apply"),
        Some(Steps {
            stage: false,
            activation: Some(Activation::Boot)
        })
    );
}

/// `steps` is the mode validation, so an unknown mode must be the only thing
/// it rejects - including the ones a client might plausibly invent.
#[test]
fn steps_rejects_unknown_modes() {
    for mode in ["", "frobnicate", "Switch", "boot ", "build-vm", "install"] {
        assert_eq!(steps(mode), None, "{mode:?}");
    }
}

/// An invalid mode must be refused before any work starts, and with the exact
/// message clients have been getting - `Error::CoreUtils`'s payload is what
/// reaches the wire, verbatim, as the `Failed` reply's message.
#[tokio::test]
async fn update_system_unknown_mode_message_is_verbatim() {
    let err = UpdateSystem.execute(&["frobnicate"]).await.unwrap_err();
    let Error::CoreUtils(message) = err else {
        panic!("expected Error::CoreUtils, got {err:?}");
    };
    assert_eq!(
        message,
        "UpdateSystem: unknown mode 'frobnicate', expected 'switch', 'boot', 'build', \
         'stage' or 'apply'"
    );
}

/// The behaviour this pipeline exists for: the only `UpdateSystem` mode that
/// activates the staged system for the next boot is `"apply"`, the explicit
/// administrative one. `"boot"` - what GNOME Software's apply job sends - must
/// stage and stop, leaving the `nixos-rebuild boot` to
/// `mx-apply-update.service` at shutdown.
#[test]
fn only_apply_promotes_for_next_boot() {
    for mode in ["build", "stage", "boot"] {
        assert_eq!(
            steps(mode).expect("known mode").activation,
            None,
            "{mode} must not activate anything"
        );
    }
    assert_eq!(
        steps("apply").expect("known mode").activation,
        Some(Activation::Boot)
    );
    assert_eq!(
        steps("switch").expect("known mode").activation,
        Some(Activation::Switch)
    );
}
