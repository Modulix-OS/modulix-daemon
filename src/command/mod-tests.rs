use super::*;

#[test]
fn registry_starts_empty() {
    assert!(registry().is_empty());
}
