use zbus::zvariant::Value;

use super::*;

fn ay_value(bytes: &[u8]) -> OwnedValue {
    OwnedValue::try_from(Value::from(bytes.to_vec())).unwrap()
}

/// Build an `"fstab"`-kind item with the six keys UDisks2 really sends.
fn fstab_item(dir: &str, opts: &str) -> ConfigItem {
    let mut details = HashMap::new();
    details.insert(
        "fsname".to_string(),
        ay_value(b"/dev/disk/by-uuid/820D-B790\0"),
    );
    details.insert("dir".to_string(), ay_value(format!("{dir}\0").as_bytes()));
    details.insert("type".to_string(), ay_value(b"vfat\0"));
    details.insert("opts".to_string(), ay_value(format!("{opts}\0").as_bytes()));
    details.insert(
        "freq".to_string(),
        OwnedValue::try_from(Value::from(0i32)).unwrap(),
    );
    details.insert(
        "passno".to_string(),
        OwnedValue::try_from(Value::from(2i32)).unwrap(),
    );

    ("fstab".to_string(), details)
}

fn entry(mount_point: &str, options: &str) -> FstabEntry {
    FstabEntry {
        mount_point: mount_point.to_string(),
        options: options.to_string(),
    }
}

#[test]
fn add_is_a_mount() {
    let items = [fstab_item("/mnt/data", "nofail,noauto")];

    assert_eq!(
        intent("AddConfigurationItem", &items),
        Some(Intent::Mount(entry("/mnt/data", "nofail,noauto")))
    );
}

#[test]
fn remove_is_an_unmount() {
    let items = [fstab_item("/mnt/data", "nofail")];

    assert_eq!(
        intent("RemoveConfigurationItem", &items),
        Some(Intent::Unmount(entry("/mnt/data", "nofail")))
    );
}

#[test]
fn update_at_the_same_mount_point_is_an_options_change() {
    let items = [
        fstab_item("/mnt/data", "nofail,noauto"),
        fstab_item("/mnt/data", "nofail,ro"),
    ];

    assert_eq!(
        intent("UpdateConfigurationItem", &items),
        Some(Intent::Options {
            current: entry("/mnt/data", "nofail,noauto"),
            new_options: "nofail,ro".to_string(),
        })
    );
}

#[test]
fn update_to_another_mount_point_is_a_move() {
    let items = [
        fstab_item("/mnt/data", "nofail"),
        fstab_item("/mnt/other", "nofail"),
    ];

    assert_eq!(
        intent("UpdateConfigurationItem", &items),
        Some(Intent::Move {
            old: entry("/mnt/data", "nofail"),
            new: entry("/mnt/other", "nofail"),
        })
    );
}

#[test]
fn update_that_only_reorders_options_asks_for_nothing() {
    let items = [
        fstab_item("/mnt/data", "nofail,noauto"),
        fstab_item("/mnt/data", "noauto,nofail"),
    ];

    assert_eq!(intent("UpdateConfigurationItem", &items), None);
}

#[test]
fn crypttab_items_are_ignored() {
    let mut details = HashMap::new();
    details.insert("name".to_string(), ay_value(b"luks-820d\0"));
    details.insert("device".to_string(), ay_value(b"/dev/nvme0n1p3\0"));
    let items = [("crypttab".to_string(), details)];

    assert_eq!(intent("AddConfigurationItem", &items), None);
}

#[test]
fn other_members_are_ignored() {
    let items = [fstab_item("/mnt/data", "nofail")];

    assert_eq!(intent("Mount", &items), None);
    assert_eq!(intent("Format", &items), None);
}

#[test]
fn wrong_item_count_is_ignored() {
    let one = [fstab_item("/mnt/data", "nofail")];
    let two = [
        fstab_item("/mnt/data", "nofail"),
        fstab_item("/mnt/other", "nofail"),
    ];

    assert_eq!(intent("UpdateConfigurationItem", &one), None);
    assert_eq!(intent("AddConfigurationItem", &two), None);
    assert_eq!(intent("AddConfigurationItem", &[]), None);
}

#[test]
fn an_item_missing_dir_is_ignored() {
    let (kind, mut details) = fstab_item("/mnt/data", "nofail");
    details.remove("dir");

    assert_eq!(intent("AddConfigurationItem", &[(kind, details)]), None);
}
