use std::collections::HashMap;

use zbus::zvariant::{OwnedValue, Value};

use super::*;

fn ay_value(bytes: &[u8]) -> OwnedValue {
    OwnedValue::try_from(Value::from(bytes.to_vec())).unwrap()
}

#[test]
fn bytes_to_string_strips_trailing_nul() {
    assert_eq!(bytes_to_string(b"/run/media/disk\0"), "/run/media/disk");
}

#[test]
fn bytes_to_string_without_nul() {
    assert_eq!(bytes_to_string(b"/run/media/disk"), "/run/media/disk");
}

#[test]
fn fstab_entry_extracts_dir_and_opts() {
    let mut details = HashMap::new();
    details.insert("dir".to_string(), ay_value(b"/mnt/data\0"));
    details.insert("opts".to_string(), ay_value(b"noatime,nofail\0"));
    let configuration = vec![("fstab".to_string(), details)];

    assert_eq!(
        fstab_entry(&configuration),
        FstabLookup::Entry(FstabEntry {
            mount_point: "/mnt/data".to_string(),
            options: "noatime,nofail".to_string(),
        })
    );
}

#[test]
fn fstab_entry_ignores_other_entries() {
    let mut details = HashMap::new();
    details.insert("options".to_string(), ay_value(b"luks\0"));
    let configuration = vec![("crypttab".to_string(), details)];

    assert_eq!(fstab_entry(&configuration), FstabLookup::Absent);
}

#[test]
fn fstab_entry_empty_configuration() {
    assert_eq!(fstab_entry(&[]), FstabLookup::Absent);
}

/// An `"fstab"` entry whose `dir` cannot be read is not the same thing as no
/// entry at all: reporting it as `Absent` would make the caller unmount a
/// still-configured device.
#[test]
fn fstab_entry_missing_dir_is_malformed_not_absent() {
    let mut details = HashMap::new();
    details.insert("opts".to_string(), ay_value(b"noatime\0"));
    let configuration = vec![("fstab".to_string(), details)];

    assert_eq!(fstab_entry(&configuration), FstabLookup::Malformed);
}

#[test]
fn fstab_entry_missing_opts_is_malformed() {
    let mut details = HashMap::new();
    details.insert("dir".to_string(), ay_value(b"/mnt/data\0"));
    let configuration = vec![("fstab".to_string(), details)];

    assert_eq!(fstab_entry(&configuration), FstabLookup::Malformed);
}

#[test]
fn fstab_entry_non_byte_array_dir_is_malformed() {
    let mut details = HashMap::new();
    details.insert(
        "dir".to_string(),
        OwnedValue::try_from(Value::from("/mnt/data")).unwrap(),
    );
    details.insert("opts".to_string(), ay_value(b"noatime\0"));
    let configuration = vec![("fstab".to_string(), details)];

    assert_eq!(fstab_entry(&configuration), FstabLookup::Malformed);
}

#[test]
fn fstab_entry_takes_the_fstab_kind_among_several() {
    let mut crypttab = HashMap::new();
    crypttab.insert("name".to_string(), ay_value(b"luks-820d\0"));

    let mut fstab = HashMap::new();
    fstab.insert("dir".to_string(), ay_value(b"/mnt/data\0"));
    fstab.insert("opts".to_string(), ay_value(b"nofail\0"));

    let configuration = vec![
        ("crypttab".to_string(), crypttab),
        ("fstab".to_string(), fstab),
    ];

    assert_eq!(
        fstab_entry(&configuration),
        FstabLookup::Entry(FstabEntry {
            mount_point: "/mnt/data".to_string(),
            options: "nofail".to_string(),
        })
    );
}

#[test]
fn same_options_ignores_order() {
    assert!(same_options("noatime,nofail", "nofail,noatime"));
}

#[test]
fn same_options_ignores_empty_segments() {
    assert!(same_options("noatime,,nofail", "nofail,noatime,"));
}

#[test]
fn same_options_ignores_duplicates() {
    assert!(same_options("nofail,nofail", "nofail"));
}

#[test]
fn same_options_detects_a_real_change() {
    assert!(!same_options("noatime,nofail", "noatime,ro"));
    assert!(!same_options("noatime", "noatime,nofail"));
    assert!(!same_options("", "nofail"));
}

#[test]
fn same_options_accepts_two_empty_lists() {
    assert!(same_options("", ""));
}
