use super::*;

#[test]
fn set_secure_boot_name_matches_method() {
    assert_eq!(SetSecureBoot.name(), "SetSecureBoot");
}

#[test]
fn both_states_parse() {
    assert!(parse_state(&["true"]).unwrap());
    assert!(!parse_state(&["false"]).unwrap());
}

#[test]
fn extra_arguments_are_ignored() {
    assert!(parse_state(&["true", "unexpected"]).unwrap());
}

#[test]
fn a_missing_state_is_refused() {
    let message = parse_state(&[]).unwrap_err().to_string();
    assert!(message.contains("missing state argument"), "{message}");
}

#[test]
fn an_unknown_state_is_refused_by_name() {
    let message = parse_state(&["yes"]).unwrap_err().to_string();
    assert!(message.contains("'yes'"), "{message}");
    assert!(message.contains("'true' or 'false'"), "{message}");
}

/// Only Setup Mode can take a new key set. The other two must be refused
/// before the transaction opens, because `limine-install.py` ignores a failed
/// `sbctl enroll-keys` and the rebuild would report success on a system whose
/// loader is left unsigned.
#[test]
fn only_setup_mode_may_enable() {
    assert!(require_enrollable(FirmwareState::SetupMode).is_ok());
    assert!(require_enrollable(FirmwareState::UserMode).is_err());
    assert!(require_enrollable(FirmwareState::NotUefi).is_err());
}

/// The refusal has to name what blocks the enrollment, since the user is the
/// only one who can clear the Platform Key.
#[test]
fn the_user_mode_refusal_says_what_to_do() {
    let message = require_enrollable(FirmwareState::UserMode)
        .unwrap_err()
        .to_string();
    assert!(message.contains("Setup Mode"), "{message}");
}

#[test]
fn the_outcome_names_the_state() {
    assert_eq!(success_message(true), "secure boot enabled");
    assert_eq!(success_message(false), "secure boot disabled");
}

/// Disabling is valid in any firmware state, so it must not go through the
/// enrollment check - a machine in User Mode has to be able to drop the
/// option.
#[tokio::test]
async fn disabling_never_checks_the_firmware() {
    let result = SetSecureBoot.execute(&["false"]).await.unwrap();
    assert_eq!(result, "secure boot disabled");
}
