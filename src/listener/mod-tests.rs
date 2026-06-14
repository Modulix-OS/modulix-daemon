use super::*;

#[test]
fn registry_contains_udisks2() {
    let names: Vec<_> = registry().iter().map(|l| l.name()).collect();
    assert_eq!(names, vec!["udisks2"]);
}
