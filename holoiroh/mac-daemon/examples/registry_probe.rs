//! Manual, run-by-hand probe: exercises the real `registry` module (Project
//! Aro PRD §8 target-resolution pipeline, row P0-4) --
//! `Registry::default_registry`/`load`/`save`/`load_or_init`/`resolve`,
//! TOML vs JSON [`ConfigFormat`] inference, and `RegistryEntry::launch_command`
//! -- against real temp files on disk, printing real output. Witnesses the
//! deterministic-route data model + alias resolution + launch primitive this
//! task adds, following this repo's no-unit-tests rule (real execution,
//! witnessed by actually running it and reading the output, same pattern as
//! `sensitive_categories_probe.rs`).
//!
//! ## What this probe does and does not witness re: the `open -b` launch
//!
//! By default this probe **constructs and prints** the deterministic
//! `open -b com.tinyspeck.slackmacgap` launch command but does **not** spawn
//! it -- so running the probe does not pop Slack open on your Mac, and the
//! probe stays idempotent. To witness the *real* launch (which will actually
//! open Slack if it's installed), run with `HOLOIROH_PROBE_REALLY_LAUNCH=1`.
//! The probe prints, honestly, which of the two paths it took.
//!
//! This probe does **not** witness a live voice/prompt path calling
//! `resolve`/`launch` on a real spoken destination -- no such wiring exists
//! in this codebase yet (see `registry.rs`'s module doc). It only witnesses
//! the data model, file I/O, resolution logic, and launch primitive this
//! pass actually implements. And note: the registry file is written
//! **plaintext**, not encrypted -- see `registry.rs`'s encryption TODO.
//!
//! Run with `cargo run --example registry_probe`
//! (or `HOLOIROH_PROBE_REALLY_LAUNCH=1 cargo run --example registry_probe`).

use holoiroh_daemon::registry::{ConfigFormat, EntryType, Registry, Resolution};

fn temp_path(name: &str, ext: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "holoiroh-registry-probe-{name}-{}-{}.{ext}",
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
    let toml_default = Registry::default_path().expect("default_path should resolve");
    let json_default = Registry::default_json_path().expect("default_json_path should resolve");
    println!("default_path()      -> {}", toml_default.display());
    println!("default_json_path() -> {}", json_default.display());
    assert_eq!(
        toml_default,
        std::path::PathBuf::from(&home).join(".holoiroh").join("registry.toml")
    );
    assert_eq!(
        json_default,
        std::path::PathBuf::from(&home).join(".holoiroh").join("registry.json")
    );

    println!();
    println!("=== default_registry: the alpha P0 verified-tier Slack native_app entry (PRD §8) ===");
    let defaults = Registry::default_registry();
    println!("entry count: {}", defaults.entries.len());
    assert_eq!(defaults.entries.len(), 1, "alpha scope: one seed entry (Slack)");
    let slack = &defaults.entries[0];
    println!(
        "  aliases={:?}\n  entry_type={:?}\n  bundle_id={:?}\n  defaults.workspace={:?}\n  policy.allowed_actions={:?} policy.remote_view_required={}",
        slack.alias,
        slack.entry_type,
        slack.bundle_id,
        slack.defaults.workspace,
        slack.policy.allowed_actions,
        slack.policy.remote_view_required,
    );
    assert_eq!(slack.entry_type, EntryType::NativeApp);
    assert_eq!(slack.bundle_id.as_deref(), Some("com.tinyspeck.slackmacgap"));
    assert!(slack.alias.iter().any(|a| a == "slack"), "Slack must resolve on the plain 'slack' alias");
    assert!(slack.defaults.workspace.is_some(), "Slack seed has a default workspace");
    assert!(
        slack.policy.allowed_actions.contains(&"send_message".to_string()),
        "PRD §8 example policy lists send_message"
    );

    println!();
    println!("=== ConfigFormat::from_path inference ===");
    let toml_p = std::path::Path::new("/tmp/x/registry.toml");
    let json_p = std::path::Path::new("/tmp/x/registry.json");
    let no_ext_p = std::path::Path::new("/tmp/x/registry");
    println!("from_path(.toml) -> {:?}", ConfigFormat::from_path(toml_p));
    println!("from_path(.json) -> {:?}", ConfigFormat::from_path(json_p));
    println!("from_path(no ext) -> {:?}", ConfigFormat::from_path(no_ext_p));
    assert_eq!(ConfigFormat::from_path(toml_p), ConfigFormat::Toml);
    assert_eq!(ConfigFormat::from_path(json_p), ConfigFormat::Json);
    assert_eq!(ConfigFormat::from_path(no_ext_p), ConfigFormat::Toml, "no extension defaults to TOML");

    println!();
    println!("=== load: missing file -> defaults, not an error ===");
    let missing = temp_path("missing", "toml");
    let loaded = Registry::load(&missing).expect("missing file should load as defaults");
    println!("load({}) -> {} entries (no file written)", missing.display(), loaded.entries.len());
    assert_eq!(loaded, Registry::default_registry());
    assert!(!missing.exists(), "load() alone must not create the file");

    println!();
    println!("=== save + load round-trip: TOML (PLAINTEXT -- see registry.rs encryption TODO) ===");
    let toml_path = temp_path("roundtrip", "toml");
    let mut to_save = Registry::default_registry();
    // Add a second, browser_url entry AND a second native_app entry that shares
    // an alias with Slack, so the round-trip proves the full schema persists and
    // sets up the ambiguity test below.
    to_save.entries.push(holoiroh_daemon::registry::RegistryEntry {
        alias: vec!["docs".to_string(), "documentation".to_string()],
        entry_type: EntryType::BrowserUrl,
        bundle_id: None,
        browser_url: Some("https://example.com/docs".to_string()),
        defaults: Default::default(),
        policy: holoiroh_daemon::registry::Policy {
            allowed_actions: vec!["read".to_string()],
            remote_view_required: true,
        },
    });
    to_save.save(&toml_path).expect("TOML save should succeed and create parent dir");
    println!("saved to {}", toml_path.display());
    let toml_contents = std::fs::read_to_string(&toml_path).unwrap();
    println!("--- file contents ---");
    println!("{}", toml_contents);
    println!("--- end ---");

    let reloaded = Registry::load(&toml_path).expect("load after save should succeed");
    assert_eq!(reloaded, to_save, "TOML round-trip must preserve every field");
    let docs = reloaded.entries.iter().find(|e| e.alias.contains(&"docs".to_string())).unwrap();
    println!(
        "reloaded browser_url entry: entry_type={:?} browser_url={:?} remote_view_required={}",
        docs.entry_type, docs.browser_url, docs.policy.remote_view_required
    );
    assert_eq!(docs.entry_type, EntryType::BrowserUrl);
    assert_eq!(docs.browser_url.as_deref(), Some("https://example.com/docs"));
    assert!(docs.policy.remote_view_required);
    let _ = std::fs::remove_file(&toml_path);

    println!();
    println!("=== save + load round-trip: JSON (format inferred from .json extension) ===");
    let json_path = temp_path("roundtrip", "json");
    let src = Registry::default_registry();
    src.save(&json_path).expect("JSON save should succeed");
    let json_contents = std::fs::read_to_string(&json_path).unwrap();
    println!("saved to {} ({} bytes)", json_path.display(), json_contents.len());
    assert!(json_contents.trim_start().starts_with('{'), "json format should actually write JSON, not TOML");
    let reloaded_json = Registry::load(&json_path).expect("load JSON after save should succeed");
    assert_eq!(reloaded_json, src, "JSON round-trip must preserve every field");
    println!("JSON round-trip: entries preserved = {}", reloaded_json == src);
    let _ = std::fs::remove_file(&json_path);

    println!();
    println!("=== load_or_init: first run writes the alpha Slack seed to disk ===");
    let init_path = temp_path("init", "toml");
    assert!(!init_path.exists());
    let inited = Registry::load_or_init(&init_path).expect("load_or_init should succeed");
    println!(
        "load_or_init({}) -> {} entries, file now exists = {}",
        init_path.display(),
        inited.entries.len(),
        init_path.exists()
    );
    assert!(init_path.exists(), "load_or_init must persist the defaults on first run");
    assert_eq!(inited, Registry::default_registry());
    // Second call must load the (possibly user-edited) file, not reset it.
    let mut existing = Registry::load(&init_path).unwrap();
    existing.entries[0].defaults.workspace = Some("acme-corp".to_string());
    existing.save(&init_path).unwrap();
    let second_call = Registry::load_or_init(&init_path).expect("second load_or_init should succeed");
    println!(
        "second load_or_init: slack.defaults.workspace={:?} (must reflect the user's edit, not reset)",
        second_call.entries[0].defaults.workspace
    );
    assert_eq!(second_call.entries[0].defaults.workspace.as_deref(), Some("acme-corp"));
    let _ = std::fs::remove_file(&init_path);

    println!();
    println!("=== corrupt file fails closed (real error, not silently-defaulted) ===");
    let corrupt_path = temp_path("corrupt", "toml");
    std::fs::write(&corrupt_path, b"this is not valid toml {{{ at all").unwrap();
    let result = Registry::load(&corrupt_path);
    println!("load(corrupt file) -> is_err={}", result.is_err());
    assert!(result.is_err(), "a corrupt registry must be a real error, not silently treated as defaults");
    if let Err(e) = &result {
        println!("  error: {e:#}");
    }
    let _ = std::fs::remove_file(&corrupt_path);

    println!();
    println!("=== resolve: SINGLE match -> deterministic target (PRD §8 steps 2-3, exactly one) ===");
    let reg = Registry::default_registry();
    // Case- and whitespace-insensitive: "  SLACK  App " must resolve to the Slack entry.
    match reg.resolve("  SLACK  App ") {
        Resolution::Single(entry) => {
            println!(
                "resolve('  SLACK  App ') -> Single -> aliases={:?} bundle_id={:?}",
                entry.alias, entry.bundle_id
            );
            assert_eq!(entry.bundle_id.as_deref(), Some("com.tinyspeck.slackmacgap"));
        }
        other => panic!("expected a single Slack match, got {other:?}"),
    }

    println!();
    println!("=== resolve: NOT FOUND -> NotFound (no autonomous guess) ===");
    let nf = reg.resolve("some destination that isn't registered");
    println!("resolve('some destination that isn't registered') -> {:?}", nf);
    assert!(matches!(nf, Resolution::NotFound));
    // Empty/whitespace-only input is also NotFound, not a spurious match.
    assert!(matches!(reg.resolve("   "), Resolution::NotFound), "blank input must be NotFound");

    println!();
    println!("=== resolve: AMBIGUOUS -> choice required, NEVER an autonomous guess (PRD §8) ===");
    // Build a registry where two DISTINCT entries share the alias "chat", to
    // force the ambiguous branch. Per the PRD this must return every candidate
    // for a user choice prompt, not silently pick one.
    let mut ambiguous_reg = Registry::default_registry();
    // Give Slack the alias "chat" too...
    ambiguous_reg.entries[0].alias.push("chat".to_string());
    // ...and add a second, distinct native_app entry also aliased "chat".
    ambiguous_reg.entries.push(holoiroh_daemon::registry::RegistryEntry {
        alias: vec!["chat".to_string(), "messages".to_string()],
        entry_type: EntryType::NativeApp,
        bundle_id: Some("com.apple.MobileSMS".to_string()),
        browser_url: None,
        defaults: Default::default(),
        policy: Default::default(),
    });
    match ambiguous_reg.resolve("chat") {
        Resolution::Ambiguous(candidates) => {
            println!("resolve('chat') -> Ambiguous with {} candidates:", candidates.len());
            for c in &candidates {
                println!("    - aliases={:?} bundle_id={:?}", c.alias, c.bundle_id);
            }
            assert_eq!(candidates.len(), 2, "both distinct entries must be surfaced for a user choice");
            let bundle_ids: Vec<&str> = candidates.iter().filter_map(|c| c.bundle_id.as_deref()).collect();
            assert!(bundle_ids.contains(&"com.tinyspeck.slackmacgap"));
            assert!(bundle_ids.contains(&"com.apple.MobileSMS"));
        }
        other => panic!("expected Ambiguous (never an autonomous pick), got {other:?}"),
    }

    println!();
    println!("=== deterministic launch (PRD §8 step 4): construct the real `open -b` command ===");
    let launch_cmd = slack.launch_command().expect("native_app with a bundle_id builds a launch command");
    println!(
        "launch_command() for Slack -> {:?} {:?}",
        launch_cmd.get_program(),
        launch_cmd.get_args().collect::<Vec<_>>()
    );
    assert_eq!(launch_cmd.get_program(), std::ffi::OsStr::new("open"));
    let args: Vec<&std::ffi::OsStr> = launch_cmd.get_args().collect();
    assert_eq!(args, vec![std::ffi::OsStr::new("-b"), std::ffi::OsStr::new("com.tinyspeck.slackmacgap")]);

    // A browser_url entry has no `open -b` route -> launch_command errors (doesn't guess).
    let browser_entry = holoiroh_daemon::registry::RegistryEntry {
        alias: vec!["docs".to_string()],
        entry_type: EntryType::BrowserUrl,
        bundle_id: None,
        browser_url: Some("https://example.com".to_string()),
        defaults: Default::default(),
        policy: Default::default(),
    };
    let browser_launch = browser_entry.launch_command();
    println!("launch_command() for a browser_url entry -> is_err={}", browser_launch.is_err());
    assert!(browser_launch.is_err(), "browser_url has no native-app `open -b` route");

    println!();
    let really_launch = std::env::var("HOLOIROH_PROBE_REALLY_LAUNCH").is_ok();
    if really_launch {
        println!("=== HOLOIROH_PROBE_REALLY_LAUNCH set: ACTUALLY running `open -b com.tinyspeck.slackmacgap` ===");
        match slack.launch() {
            Ok(()) => println!("launch() -> Ok (Slack was asked to open via LaunchServices)"),
            Err(e) => println!("launch() -> Err (likely Slack not installed): {e:#}"),
        }
    } else {
        println!("=== deterministic launch NOT actually run (default) ===");
        println!(
            "Set HOLOIROH_PROBE_REALLY_LAUNCH=1 to actually spawn `open -b com.tinyspeck.slackmacgap` \
             (will open Slack if installed). This run only CONSTRUCTED and inspected the command."
        );
    }

    println!();
    println!("All registry probes passed.");
    println!();
    println!(
        "NOTE: this probe witnesses the data model, file I/O, alias resolution, and the \
         deterministic `open -b` launch primitive added in this pass. The registry file is \
         written PLAINTEXT, not encrypted (see registry.rs's encryption TODO). No live \
         voice/prompt path calls resolve()/launch() on a real spoken destination yet -- \
         see registry.rs's module doc."
    );
}
