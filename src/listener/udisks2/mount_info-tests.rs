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
        Some(FstabEntry {
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

    assert_eq!(fstab_entry(&configuration), None);
}

#[test]
fn fstab_entry_empty_configuration() {
    assert_eq!(fstab_entry(&[]), None);
}

#[test]
fn fstab_entry_missing_dir() {
    let mut details = HashMap::new();
    details.insert("opts".to_string(), ay_value(b"noatime\0"));
    let configuration = vec![("fstab".to_string(), details)];

    assert_eq!(fstab_entry(&configuration), None);
}
