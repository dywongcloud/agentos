//! Manual, run-by-hand probe: exercises the real `allowlist` module --
//! `Allowlist::load`/`save`/`add_entry`/`remove_entry`/`contains_key`,
//! `generate_pin`/`generate_default_pin`, and `verify_pin` -- against real
//! temp files on disk and real strings, printing real output. Witnesses
//! the behavior that used to live in `allowlist.rs`'s
//! `#[cfg(test)] mod tests` (removed per this repo's no-unit-tests rule:
//! all validation must be real execution, witnessed by actually running it
//! and reading the output), including the security-relevant "corrupt file
//! fails closed" case.
//!
//! Run with `cargo run --example allowlist_probe`.

use holoiroh_daemon::allowlist::{generate_default_pin, generate_pin, verify_pin, Allowlist};

fn temp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "holoiroh-allowlist-probe-{name}-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn main() {
    println!("=== Allowlist: load missing file -> empty, not an error ===");
    let path = temp_path("missing");
    let list = Allowlist::load(&path).expect("missing file should load as empty, not error");
    println!("load({}) -> is_empty={} len={}", path.display(), list.is_empty(), list.len());
    assert!(list.is_empty());
    assert_eq!(list.len(), 0);

    println!();
    println!("=== Allowlist: save then load round-trips entries ===");
    let path = temp_path("roundtrip");
    let mut list = Allowlist::default();
    let added1 = list.add_entry("node-abc123", Some("Dylan's iPhone".to_string()));
    let added2 = list.add_entry("node-def456", None);
    println!("add_entry(node-abc123) -> {added1}, add_entry(node-def456) -> {added2}");
    list.save(&path).expect("save should succeed and create parent dir");
    println!("saved to {}", path.display());

    let loaded = Allowlist::load(&path).expect("load should succeed after save");
    println!(
        "loaded: len={} contains(node-abc123)={} contains(node-def456)={} contains(node-unknown)={}",
        loaded.len(),
        loaded.contains_key("node-abc123"),
        loaded.contains_key("node-def456"),
        loaded.contains_key("node-unknown")
    );
    assert_eq!(loaded.len(), 2);
    assert!(loaded.contains_key("node-abc123"));
    assert!(loaded.contains_key("node-def456"));
    assert!(!loaded.contains_key("node-unknown"));
    let _ = std::fs::remove_file(&path);

    println!();
    println!("=== Allowlist: add_entry is idempotent for the same device_id ===");
    let mut list = Allowlist::default();
    let first_add = list.add_entry("node-x", None);
    let second_add = list.add_entry("node-x", Some("relabel attempt".to_string()));
    println!("first add -> {first_add}, second add (same id) -> {second_add}, len={}", list.len());
    assert!(first_add);
    assert!(!second_add);
    assert_eq!(list.len(), 1);

    println!();
    println!("=== Allowlist: remove_entry revokes a previously paired device ===");
    let mut list = Allowlist::default();
    list.add_entry("node-to-revoke", None);
    let contained_before = list.contains_key("node-to-revoke");
    let removed = list.remove_entry("node-to-revoke");
    let contained_after = list.contains_key("node-to-revoke");
    let removed_again = list.remove_entry("node-to-revoke");
    println!(
        "before={contained_before} removed={removed} after={contained_after} removed_again={removed_again}"
    );
    assert!(contained_before);
    assert!(removed);
    assert!(!contained_after);
    assert!(!removed_again, "second removal of an already-gone entry must report no-op");

    println!();
    println!("=== Allowlist: corrupt JSON file fails CLOSED, not open (security-relevant) ===");
    let path = temp_path("corrupt");
    std::fs::write(&path, b"{ this is not valid json").unwrap();
    let result = Allowlist::load(&path);
    println!("load(corrupt file) -> is_err={}", result.is_err());
    assert!(
        result.is_err(),
        "a corrupt allowlist file must be a hard error, never silently treated as empty \
         (empty would fail OPEN -- accepting a device that was never actually verified)"
    );
    let _ = std::fs::remove_file(&path);

    println!();
    println!("=== Allowlist::default_path ===");
    if std::env::var_os("HOME").is_some() {
        let path = Allowlist::default_path().unwrap();
        println!("default_path() -> {}", path.display());
        assert!(path.ends_with(".holoiroh/allowlist.json"));
    } else {
        println!("HOME not set in this environment -- skipping (matches the original test's own guard)");
    }

    println!();
    println!("=== generate_pin / generate_default_pin ===");
    let pin6 = generate_pin(6);
    println!("generate_pin(6) -> {pin6:?} (len={}, all_digits={})", pin6.len(), pin6.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(pin6.len(), 6);
    assert!(pin6.chars().all(|c| c.is_ascii_digit()));

    let default_pin = generate_default_pin();
    println!("generate_default_pin() -> {default_pin:?} (len={})", default_pin.len());
    assert_eq!(default_pin.len(), 6);

    let pin0 = generate_pin(0);
    println!("generate_pin(0) -> {pin0:?} (clamps to 1 digit, len={})", pin0.len());
    assert_eq!(pin0.len(), 1);

    println!();
    println!("=== verify_pin ===");
    let cases: &[(&str, &str, &str, bool)] = &[
        ("correct match", "123456", "123456", true),
        ("wrong pin", "000000", "123456", false),
        ("empty candidate", "", "123456", false),
        ("empty expected", "123456", "", false),
        ("both empty", "", "", false),
        ("candidate shorter", "123", "123456", false),
        ("candidate longer", "123456", "123", false),
        ("off by one digit", "123457", "123456", false),
        ("case sensitive: wrong case", "abcdef", "ABCDEF", false),
        ("case sensitive: same case", "abcdef", "abcdef", true),
    ];
    for (label, candidate, expected_pin, expected_result) in cases {
        let result = verify_pin(candidate, expected_pin);
        println!(
            "verify_pin({candidate:?}, {expected_pin:?}) -> {result} [{label}]"
        );
        assert_eq!(result, *expected_result, "mismatch for case: {label}");
    }

    println!();
    println!("allowlist_probe: OK -- all Allowlist/PIN cases witnessed via real execution");
}
