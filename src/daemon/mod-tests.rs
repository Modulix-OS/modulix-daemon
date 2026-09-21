//! Every `Daemon` method now checks polkit authorization for the caller
//! carried in the D-Bus message header before dispatching to its `Command`
//! (see `run`); that requires a live system bus + polkit authority, so it is
//! exercised by `busctl` (see the daemon's README/CLAUDE.md "Vérification"
//! section) rather than by a unit test here. `Command::execute` itself —
//! what each method delegates to — is covered directly by
//! `command/*-tests.rs`.

use super::*;

#[test]
fn bus_name_matches_deployed_name() {
    assert_eq!(BUS_NAME, "org.modulix.Daemon");
}

#[test]
fn object_path_is_well_formed() {
    assert!(OBJECT_PATH.starts_with('/'));
}
