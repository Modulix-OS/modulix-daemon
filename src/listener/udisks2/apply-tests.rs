use super::*;

#[test]
fn option_list_splits_on_commas() {
    assert_eq!(
        option_list("noatime,nofail,ro"),
        vec!["noatime", "nofail", "ro"]
    );
}

#[test]
fn option_list_drops_empty_segments() {
    assert_eq!(option_list("noatime,,nofail,"), vec!["noatime", "nofail"]);
    assert_eq!(option_list(""), Vec::<String>::new());
    assert_eq!(option_list(",,"), Vec::<String>::new());
}

#[test]
fn option_list_trims_each_option() {
    assert_eq!(option_list(" noatime , nofail "), vec!["noatime", "nofail"]);
}

/// The order is the user's; it is written back verbatim rather than
/// normalised.
#[test]
fn option_list_keeps_order_and_duplicates() {
    assert_eq!(option_list("ro,noatime,ro"), vec!["ro", "noatime", "ro"]);
}
