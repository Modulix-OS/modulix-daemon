use super::*;
use crate::listener::udisks2::mount_info::FstabEntry;

fn serial(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

fn path(n: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("/org/freedesktop/UDisks2/block_devices/{n}")).unwrap()
}

fn mount(mount_point: &str) -> Intent {
    Intent::Mount(FstabEntry {
        mount_point: mount_point.to_string(),
        options: "nofail".to_string(),
    })
}

#[test]
fn a_reply_takes_back_its_own_call() {
    let mut calls = PendingCalls::default();
    calls.record(serial(7), path("sda1"), mount("/mnt/data"));

    assert_eq!(
        calls.take(serial(7)),
        Some((path("sda1"), mount("/mnt/data")))
    );
}

#[test]
fn a_call_is_taken_back_only_once() {
    let mut calls = PendingCalls::default();
    calls.record(serial(7), path("sda1"), mount("/mnt/data"));

    assert!(calls.take(serial(7)).is_some());
    assert!(calls.take(serial(7)).is_none());
}

#[test]
fn an_unknown_reply_serial_is_ignored() {
    let mut calls = PendingCalls::default();
    calls.record(serial(7), path("sda1"), mount("/mnt/data"));

    assert!(calls.take(serial(8)).is_none());
    assert!(calls.take(serial(7)).is_some());
}

#[test]
fn calls_are_kept_apart_by_serial() {
    let mut calls = PendingCalls::default();
    calls.record(serial(1), path("sda1"), mount("/mnt/one"));
    calls.record(serial(2), path("sdb1"), mount("/mnt/two"));

    assert_eq!(
        calls.take(serial(2)),
        Some((path("sdb1"), mount("/mnt/two")))
    );
    assert_eq!(
        calls.take(serial(1)),
        Some((path("sda1"), mount("/mnt/one")))
    );
}

#[test]
fn expired_calls_are_dropped_on_the_next_record() {
    let mut calls = PendingCalls::default();
    calls.0.insert(
        serial(7),
        Pending {
            path: path("sda1"),
            intent: mount("/mnt/data"),
            at: Instant::now() - PENDING_TTL - Duration::from_secs(1),
        },
    );

    calls.record(serial(8), path("sdb1"), mount("/mnt/other"));

    assert!(calls.take(serial(7)).is_none());
    assert!(calls.take(serial(8)).is_some());
}

#[test]
fn recording_past_the_cap_evicts_the_oldest() {
    let mut calls = PendingCalls::default();
    let now = Instant::now();

    for n in 1..=MAX_PENDING as u32 {
        calls.0.insert(
            serial(n),
            Pending {
                path: path("sda1"),
                intent: mount("/mnt/data"),
                // Serial 1 is the oldest, MAX_PENDING the youngest.
                at: now - Duration::from_secs((MAX_PENDING as u64) - u64::from(n)),
            },
        );
    }

    calls.record(
        serial(MAX_PENDING as u32 + 1),
        path("sdb1"),
        mount("/mnt/new"),
    );

    assert_eq!(calls.0.len(), MAX_PENDING);
    assert!(calls.take(serial(1)).is_none());
    assert!(calls.take(serial(MAX_PENDING as u32 + 1)).is_some());
}
