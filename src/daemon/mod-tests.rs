use super::*;

#[test]
fn bus_name_matches_deployed_name() {
    assert_eq!(BUS_NAME, "org.modulix.Daemon");
}

#[test]
fn object_path_is_well_formed() {
    assert!(OBJECT_PATH.starts_with('/'));
}
