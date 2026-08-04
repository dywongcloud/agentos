//! Executable allowlist/PIN and legacy-ID migration probe.
//!
//! Run with `cargo run --example allowlist_probe`.

use holoiroh_daemon::allowlist::{
    Allowlist, generate_default_pin, generate_pin, migration_backup_path, verify_pin,
};

fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "holoiroh-allowlist-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn endpoint_id(prefix: &str, fill: char) -> String {
    assert!(prefix.len() <= 64);
    format!("{prefix}{}", fill.to_string().repeat(64 - prefix.len()))
}

fn main() {
    let root = temp_dir();
    std::fs::create_dir_all(&root).unwrap();

    println!("=== missing allowlist is empty ===");
    let missing = root.join("missing.json");
    let list = Allowlist::load(&missing).expect("missing file must load empty");
    assert!(list.is_empty());

    let valid_a = endpoint_id("0123456789", 'a');
    let valid_b = endpoint_id("0123456789", 'b');
    let prefix_collision = endpoint_id("0123456789", 'c');
    let truncated = "0123456789".to_string();
    let uppercase = "A".repeat(64);
    let malformed = endpoint_id("012345678g", 'd');

    println!("=== mixed legacy data migrates atomically to active + quarantine ===");
    let path = root.join("allowlist.json");
    let mixed = serde_json::json!({
        "entries": [
            {"device_id": valid_a.clone(), "label": "valid-a", "paired_at": 1},
            {"device_id": truncated.clone(), "label": "legacy-short", "paired_at": 2},
            {"device_id": uppercase.clone(), "label": "uppercase", "paired_at": 3},
            {"device_id": malformed.clone(), "label": "malformed", "paired_at": 4},
            {"device_id": valid_b.clone(), "label": "valid-b", "paired_at": 5}
        ]
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&mixed).unwrap()).unwrap();

    let migrated = Allowlist::load(&path).expect("mixed legacy file must migrate");
    assert_eq!(migrated.len(), 2);
    assert!(migrated.contains_key(&valid_a));
    assert!(migrated.contains_key(&valid_b));
    assert!(!migrated.contains_key(&prefix_collision));
    assert!(!migrated.contains_key("0123456789"));

    let backup = migration_backup_path(&path);
    let backup_value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&backup).expect("migration backup missing")).unwrap();
    let backup_ids: Vec<&str> = backup_value["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["device_id"].as_str().unwrap())
        .collect();
    assert_eq!(backup_ids, vec![truncated, uppercase, malformed]);
    println!(
        "migration OK: valid=2 quarantined=3 backup={}",
        backup.display()
    );

    println!("=== second load is byte-idempotent ===");
    let active_before = std::fs::read(&path).unwrap();
    let backup_before = std::fs::read(&backup).unwrap();
    let second = Allowlist::load(&path).expect("second load must succeed");
    assert_eq!(second.len(), 2);
    assert_eq!(std::fs::read(&path).unwrap(), active_before);
    assert_eq!(std::fs::read(&backup).unwrap(), backup_before);

    println!("=== exact full-ID matching rejects a shared-prefix collision ===");
    assert_eq!(&valid_a[..10], &valid_b[..10]);
    assert_eq!(&valid_a[..10], &prefix_collision[..10]);
    assert!(migrated.contains_key(&valid_a));
    assert!(migrated.contains_key(&valid_b));
    assert!(!migrated.contains_key(&prefix_collision));

    println!("=== add/save accept only complete lowercase endpoint IDs ===");
    let roundtrip = root.join("roundtrip.json");
    let mut list = Allowlist::default();
    assert!(list.add_entry(valid_a.clone(), Some("phone".to_string())));
    assert!(!list.add_entry(valid_a.clone(), None));
    assert!(!list.add_entry("0123456789", None));
    assert!(!list.add_entry("F".repeat(64), None));
    list.save(&roundtrip).unwrap();
    assert!(Allowlist::load(&roundtrip).unwrap().contains_key(&valid_a));

    println!("=== failed replace leaves no partial active file or temp residue ===");
    let failure_parent = root.join("failure");
    let destination = failure_parent.join("allowlist.json");
    std::fs::create_dir_all(&destination).unwrap();
    let result = list.save(&destination);
    assert!(result.is_err(), "renaming over a directory must fail");
    assert!(
        destination.is_dir(),
        "failed save must preserve the destination"
    );
    let temp_prefix = ".allowlist.json.tmp-";
    assert!(
        std::fs::read_dir(&failure_parent)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(temp_prefix)),
        "failed atomic save must clean its temporary file"
    );

    println!("=== corrupt JSON fails closed ===");
    let corrupt = root.join("corrupt.json");
    std::fs::write(&corrupt, b"{ this is not valid json").unwrap();
    assert!(Allowlist::load(&corrupt).is_err());

    println!("=== PIN helpers ===");
    let pin6 = generate_pin(6);
    assert_eq!(pin6.len(), 6);
    assert!(pin6.bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(generate_default_pin().len(), 6);
    assert_eq!(generate_pin(0).len(), 1);
    assert!(verify_pin("123456", "123456"));
    for candidate in ["000000", "", "123", "1234567"] {
        assert!(!verify_pin(candidate, "123456"));
    }

    std::fs::remove_dir_all(&root).unwrap();
    println!("allowlist_probe: ALL CHECKS PASSED");
}
