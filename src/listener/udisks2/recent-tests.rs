use super::*;

fn path(n: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("/org/freedesktop/UDisks2/block_devices/{n}")).unwrap()
}

#[test]
fn unstamped_path_is_not_an_echo() {
    assert!(!is_echo(&path("never_stamped")));
}

#[test]
fn stamped_path_is_an_echo() {
    let p = path("stamped");
    record(&p);

    assert!(is_echo(&p));
}

#[test]
fn stamping_one_path_does_not_stamp_another() {
    let stamped = path("one");
    record(&stamped);

    assert!(is_echo(&stamped));
    assert!(!is_echo(&path("other")));
}

#[test]
fn expired_stamps_are_pruned() {
    let p = path("expired");
    RECENT.lock().expect("lock poisoned").insert(
        p.clone(),
        Instant::now() - ECHO_WINDOW - Duration::from_secs(1),
    );

    assert!(!is_echo(&p));
    assert!(!RECENT.lock().expect("lock poisoned").contains_key(&p));
}
