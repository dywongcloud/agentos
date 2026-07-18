//! Manual, run-by-hand probe: exercises the real `auth::extract_api_key`
//! and `auth::check_holo_token_in` functions against real strings and a
//! real temp directory/file on disk, printing real output -- witnesses the
//! parsing and token-file-resolution behavior that used to live in
//! `auth.rs`'s `#[cfg(test)] mod tests` (removed per this repo's
//! no-unit-tests rule: all validation must be real execution, witnessed by
//! actually running it and reading the output).
//!
//! Run with `cargo run --example auth_probe`.

use holoiroh_daemon::auth::{check_holo_token_in, extract_api_key};

fn check(label: &str, contents: &str, expected: Option<&str>) {
    let result = extract_api_key(contents);
    println!("{label}: input={contents:?} -> {result:?}");
    assert_eq!(
        result.as_deref(),
        expected,
        "{label}: extract_api_key mismatch"
    );
}

fn main() {
    println!("=== extract_api_key parsing (real function, real inputs) ===");
    check("basic", "HAI_API_KEY=hk-abc123\n", Some("hk-abc123"));
    check(
        "quoted",
        "HAI_API_KEY=\"hk-abc123\"\n",
        Some("hk-abc123"),
    );
    check(
        "single-quoted",
        "HAI_API_KEY='hk-abc123'\n",
        Some("hk-abc123"),
    );
    check(
        "comments_and_blank_lines",
        "# comment\n\nHAI_API_KEY=hk-xyz\n",
        Some("hk-xyz"),
    );
    check("missing", "SOME_OTHER_VAR=1\n", None);
    check("empty_value", "HAI_API_KEY=\n", Some(""));

    println!();
    println!("=== check_holo_token_in: real filesystem I/O ===");

    // Missing token file.
    let missing_dir = std::env::temp_dir().join(format!(
        "holoiroh-auth-probe-missing-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&missing_dir);
    std::fs::create_dir_all(&missing_dir).expect("create missing_dir");
    let result = check_holo_token_in(&missing_dir);
    println!("missing token file -> {result:?}");
    assert!(result.is_err(), "expected an error for a missing token file");
    let _ = std::fs::remove_dir_all(&missing_dir);

    // Happy path: real file, real key.
    let happy_dir = std::env::temp_dir().join(format!(
        "holoiroh-auth-probe-happy-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&happy_dir);
    let holo_dir = happy_dir.join(".holo");
    std::fs::create_dir_all(&holo_dir).expect("create .holo dir");
    std::fs::write(holo_dir.join(".env"), "HAI_API_KEY=hk-probe-token\n").expect("write .env");
    let token = check_holo_token_in(&happy_dir).expect("token should parse");
    println!(
        "happy path -> api_key={:?} path={}",
        token.api_key(),
        token.path().display()
    );
    assert_eq!(token.api_key(), "hk-probe-token");
    let _ = std::fs::remove_dir_all(&happy_dir);

    // Empty file -> missing key.
    let empty_dir = std::env::temp_dir().join(format!(
        "holoiroh-auth-probe-empty-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&empty_dir);
    let holo_dir = empty_dir.join(".holo");
    std::fs::create_dir_all(&holo_dir).expect("create .holo dir");
    std::fs::write(holo_dir.join(".env"), "").expect("write empty .env");
    let result = check_holo_token_in(&empty_dir);
    println!("empty .env file -> {result:?}");
    assert!(result.is_err(), "expected an error for an empty .env file");
    let _ = std::fs::remove_dir_all(&empty_dir);

    println!();
    println!(
        "auth_probe: OK -- all extract_api_key/check_holo_token_in cases witnessed via real execution"
    );
}
