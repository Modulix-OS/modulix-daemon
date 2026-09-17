use super::*;

#[test]
fn resolve_none_falls_back_to_compile_time_constant() {
    assert_eq!(resolve(None), modulix_core_utils::CONFIG_DIRECTORY);
}

#[test]
fn resolve_empty_falls_back_to_compile_time_constant() {
    assert_eq!(resolve(Some("")), modulix_core_utils::CONFIG_DIRECTORY);
}

#[test]
fn resolve_without_trailing_slash_appends_one() {
    assert_eq!(resolve(Some("/x/y")), "/x/y/");
}

#[test]
fn resolve_with_trailing_slash_is_idempotent() {
    assert_eq!(resolve(Some("/x/y/")), "/x/y/");
}
