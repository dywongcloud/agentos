//! Manual, run-by-hand probe: exercises the real `sensitive_categories` module --
//! `SensitiveCategories::default_categories`/`load`/`save`/`load_or_init`/`classify`/
//! `find_by_id`/`find_by_id_mut`, and TOML vs JSON [`ConfigFormat`] inference -- against
//! real temp files on disk, printing real output. Witnesses the config-file data model
//! this task adds, following this repo's no-unit-tests rule (real execution, witnessed
//! by actually running it and reading the output, same pattern as `allowlist_probe.rs`
//! and `auth_gate_probe.rs`).
//!
//! This probe does **not** and cannot witness a live policy-interception point pausing
//! an agent action before it touches a sensitive app -- no such interception point
//! exists in this codebase yet (see `sensitive_categories.rs`'s own module doc, "What
//! this module is not"). It only witnesses the data model and file I/O this pass
//! actually implements.
//!
//! Run with `cargo run --example sensitive_categories_probe`.

use holoiroh_daemon::sensitive_categories::{CategorySetting, ConfigFormat, SensitiveCategories};

fn temp_path(name: &str, ext: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "holoiroh-sensitive-categories-probe-{name}-{}-{}.{ext}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

fn main() {
    println!("=== default_path / default_json_path resolve under $HOME/.holoiroh (no file written) ===");
    let home = std::env::var("HOME").expect("HOME must be set to run this probe");
    let toml_default = SensitiveCategories::default_path().expect("default_path should resolve");
    let json_default = SensitiveCategories::default_json_path().expect("default_json_path should resolve");
    println!("default_path()      -> {}", toml_default.display());
    println!("default_json_path() -> {}", json_default.display());
    assert_eq!(
        toml_default,
        std::path::PathBuf::from(&home).join(".holoiroh").join("sensitive_categories.toml")
    );
    assert_eq!(
        json_default,
        std::path::PathBuf::from(&home).join(".holoiroh").join("sensitive_categories.json")
    );

    println!();
    println!("=== default_categories: PRD §9's eight categories, all AlwaysAsk ===");
    let defaults = SensitiveCategories::default_categories();
    println!("category count: {}", defaults.categories.len());
    for c in &defaults.categories {
        println!(
            "  {:<28} \"{}\" -- {} bundle id(s), setting={:?}",
            c.id,
            c.display_name,
            c.bundle_ids.len(),
            c.setting
        );
        assert_eq!(c.setting, CategorySetting::AlwaysAsk, "PRD default is always-ask");
        assert!(!c.bundle_ids.is_empty(), "{} should have at least one illustrative bundle id", c.id);
    }
    let expected_ids = [
        "password_managers",
        "banking_brokerage",
        "payroll_tax_legal",
        "health",
        "system_security_settings",
        "identity_admin_consoles",
        "device_management",
        "production_infra",
    ];
    assert_eq!(defaults.categories.len(), expected_ids.len());
    for id in expected_ids {
        assert!(defaults.find_by_id(id).is_some(), "missing expected category {id}");
    }

    println!();
    println!("=== classify: bundle-ID heuristic lookup ===");
    let onepassword = defaults.classify("com.1password.1password");
    let chrome_in_production_infra = defaults.classify("com.google.Chrome");
    let unknown = defaults.classify("com.example.totally-unrelated-app");
    println!("classify(com.1password.1password) -> {:?}", onepassword.map(|c| &c.id));
    println!("classify(com.google.Chrome) -> {:?}", chrome_in_production_infra.map(|c| &c.id));
    println!("classify(com.example.totally-unrelated-app) -> {:?}", unknown.map(|c| &c.id));
    assert_eq!(onepassword.map(|c| c.id.as_str()), Some("password_managers"));
    assert_eq!(chrome_in_production_infra.map(|c| c.id.as_str()), Some("production_infra"));
    assert!(unknown.is_none(), "an unrelated bundle id must not classify into any category");

    println!();
    println!("=== ConfigFormat::from_path inference ===");
    let toml_path = std::path::Path::new("/tmp/x/sensitive_categories.toml");
    let json_path = std::path::Path::new("/tmp/x/sensitive_categories.json");
    let no_ext_path = std::path::Path::new("/tmp/x/sensitive_categories");
    println!("from_path(.toml) -> {:?}", ConfigFormat::from_path(toml_path));
    println!("from_path(.json) -> {:?}", ConfigFormat::from_path(json_path));
    println!("from_path(no ext) -> {:?}", ConfigFormat::from_path(no_ext_path));
    assert_eq!(ConfigFormat::from_path(toml_path), ConfigFormat::Toml);
    assert_eq!(ConfigFormat::from_path(json_path), ConfigFormat::Json);
    assert_eq!(ConfigFormat::from_path(no_ext_path), ConfigFormat::Toml, "no extension defaults to TOML");

    println!();
    println!("=== load: missing file -> defaults, not an error ===");
    let missing = temp_path("missing", "toml");
    let loaded = SensitiveCategories::load(&missing).expect("missing file should load as defaults");
    println!("load({}) -> {} categories (no file written)", missing.display(), loaded.categories.len());
    assert_eq!(loaded, SensitiveCategories::default_categories());
    assert!(!missing.exists(), "load() alone must not create the file");

    println!();
    println!("=== save + load round-trip: TOML ===");
    let toml_path = temp_path("roundtrip", "toml");
    let mut to_save = SensitiveCategories::default_categories();
    // Mutate one category's setting so the round-trip actually proves the setting
    // persists, not just the bundle-id lists.
    to_save.find_by_id_mut("banking_brokerage").unwrap().setting = CategorySetting::HardBlock;
    to_save.find_by_id_mut("production_infra").unwrap().setting = CategorySetting::AlwaysAllow;
    to_save.save(&toml_path).expect("TOML save should succeed and create parent dir");
    println!("saved to {}", toml_path.display());
    let toml_contents = std::fs::read_to_string(&toml_path).unwrap();
    println!("--- file contents (first 400 chars) ---");
    println!("{}", &toml_contents[..toml_contents.len().min(400)]);
    println!("--- end excerpt ---");

    let reloaded = SensitiveCategories::load(&toml_path).expect("load after save should succeed");
    println!(
        "reloaded: banking_brokerage.setting={:?} production_infra.setting={:?} password_managers.setting={:?}",
        reloaded.find_by_id("banking_brokerage").unwrap().setting,
        reloaded.find_by_id("production_infra").unwrap().setting,
        reloaded.find_by_id("password_managers").unwrap().setting,
    );
    assert_eq!(reloaded.find_by_id("banking_brokerage").unwrap().setting, CategorySetting::HardBlock);
    assert_eq!(reloaded.find_by_id("production_infra").unwrap().setting, CategorySetting::AlwaysAllow);
    assert_eq!(reloaded.find_by_id("password_managers").unwrap().setting, CategorySetting::AlwaysAsk);
    let _ = std::fs::remove_file(&toml_path);

    println!();
    println!("=== save + load round-trip: JSON (format inferred from .json extension) ===");
    let json_path = temp_path("roundtrip", "json");
    let mut to_save = SensitiveCategories::default_categories();
    to_save.find_by_id_mut("health").unwrap().setting = CategorySetting::HardBlock;
    to_save.save(&json_path).expect("JSON save should succeed");
    let json_contents = std::fs::read_to_string(&json_path).unwrap();
    println!("saved to {} ({} bytes)", json_path.display(), json_contents.len());
    assert!(json_contents.trim_start().starts_with('{'), "json format should actually write JSON, not TOML");
    let reloaded_json = SensitiveCategories::load(&json_path).expect("load JSON after save should succeed");
    println!("reloaded: health.setting={:?}", reloaded_json.find_by_id("health").unwrap().setting);
    assert_eq!(reloaded_json.find_by_id("health").unwrap().setting, CategorySetting::HardBlock);
    let _ = std::fs::remove_file(&json_path);

    println!();
    println!("=== load_or_init: first run writes sensible defaults to disk ===");
    let init_path = temp_path("init", "toml");
    assert!(!init_path.exists());
    let inited = SensitiveCategories::load_or_init(&init_path).expect("load_or_init should succeed");
    println!(
        "load_or_init({}) -> {} categories, file now exists = {}",
        init_path.display(),
        inited.categories.len(),
        init_path.exists()
    );
    assert!(init_path.exists(), "load_or_init must persist the defaults on first run");
    assert_eq!(inited, SensitiveCategories::default_categories());

    // Second call: file already exists, so this must load what's there (including any
    // edits), not silently overwrite with fresh defaults.
    let mut existing = SensitiveCategories::load(&init_path).unwrap();
    existing.find_by_id_mut("identity_admin_consoles").unwrap().setting = CategorySetting::HardBlock;
    existing.save(&init_path).unwrap();
    let second_call = SensitiveCategories::load_or_init(&init_path).expect("second load_or_init should succeed");
    println!(
        "second load_or_init call: identity_admin_consoles.setting={:?} (must reflect the user's edit, not reset)",
        second_call.find_by_id("identity_admin_consoles").unwrap().setting
    );
    assert_eq!(
        second_call.find_by_id("identity_admin_consoles").unwrap().setting,
        CategorySetting::HardBlock,
        "load_or_init must not clobber an existing user-edited file"
    );
    let _ = std::fs::remove_file(&init_path);

    println!();
    println!("=== corrupt file fails closed (real error, not silently-defaulted) ===");
    let corrupt_path = temp_path("corrupt", "toml");
    std::fs::write(&corrupt_path, b"this is not valid toml {{{ at all").unwrap();
    let result = SensitiveCategories::load(&corrupt_path);
    println!("load(corrupt file) -> is_err={}", result.is_err());
    assert!(result.is_err(), "a corrupt config must be a real error, not silently treated as defaults");
    if let Err(e) = &result {
        println!("  error: {e:#}");
    }
    let _ = std::fs::remove_file(&corrupt_path);

    println!();
    println!("=== all_bundle_ids: cross-category dedup diagnostic ===");
    let all_ids = defaults.all_bundle_ids();
    let total_listed: usize = defaults.categories.iter().map(|c| c.bundle_ids.len()).sum();
    println!("total bundle ids listed across categories: {total_listed}, deduplicated: {}", all_ids.len());
    assert_eq!(all_ids.len(), total_listed, "default lists are disjoint, so dedup should not drop anything");

    println!();
    println!("All sensitive_categories probes passed.");
    println!();
    println!(
        "NOTE: this probe only witnesses the data model and file I/O added in this pass. \
         No live policy-interception point exists yet to actually pause an agent action \
         before a sensitive app -- see sensitive_categories.rs's module doc."
    );
}
