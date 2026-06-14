use super::*;

#[test]
fn name_is_udisks2() {
    assert_eq!(Udisks2Listener.name(), "udisks2");
}
