use super::*;

#[tokio::test]
async fn apply_option_sets_value() {
    let result = apply_option(&("theme".to_string(), "dark".to_string(), false))
        .await
        .unwrap();
    assert_eq!(result, "option theme set to dark");
}

#[tokio::test]
async fn apply_option_resets_to_default() {
    let result = apply_option(&("theme".to_string(), String::new(), true))
        .await
        .unwrap();
    assert_eq!(result, "option theme reset to default");
}

#[tokio::test]
async fn apply_list_sets_entry() {
    let result = apply_list(&("favorites".to_string(), "vim".to_string(), false))
        .await
        .unwrap();
    assert_eq!(result, "list favorites entry set to vim");
}

#[tokio::test]
async fn apply_list_resets_to_default() {
    let result = apply_list(&("favorites".to_string(), String::new(), true))
        .await
        .unwrap();
    assert_eq!(result, "list favorites reset to default");
}
