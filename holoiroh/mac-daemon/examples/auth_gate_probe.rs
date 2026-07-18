//! Manual, run-by-hand probe: exercises the real `ControlChannel::authenticate` PIN/allowlist
//! gate directly, against a real in-memory `AuthState` (built via the real `AuthState`, not a
//! reimplementation) and a real `tokio::io::Lines` reader over an in-memory byte buffer, printing
//! real accept/reject output for each case. Witnesses the async gate logic that used to live in
//! `control_channel.rs`'s `#[cfg(test)] mod tests` `#[tokio::test]` fns (removed per this repo's
//! no-unit-tests rule) -- same seam those tests used (`authenticate` takes `&Arc<Mutex<AuthState>>`
//! explicitly so it's callable without a real `Arc<HoloBridge>`/live `holo serve` subprocess),
//! just driven by `cargo run` instead of `cargo test`.
//!
//! This probe covers the gate logic itself, in isolation. The full, real-network path (an actual
//! `iroh` dial against a real running daemon, PIN accepted/rejected end-to-end) is separately
//! witnessed by `examples/control_probe.rs` against a live `holoiroh-daemon` process -- see this
//! task's session notes for why that live-daemon witness could not be completed in this sandbox
//! (real, observed blocker: this session's `holoiroh-daemon` process exits immediately with
//! "Missing permission: Accessibility" because this non-interactive sandboxed session has no
//! macOS Accessibility TCC grant, and `TCC.db` itself returns "authorization denied" when queried
//! directly -- there is no way to grant or bypass this from within the sandbox).
//!
//! Run with `cargo run --example auth_gate_probe`.

use std::sync::Arc;

use holoiroh_daemon::control_channel::{AuthState, ControlChannel};
use tokio::io::AsyncBufReadExt;

fn lines_from(input: &'static str) -> tokio::io::Lines<std::io::Cursor<&'static [u8]>> {
    AsyncBufReadExt::lines(std::io::Cursor::new(input.as_bytes()))
}

fn probe_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "holoiroh-auth-gate-probe-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[tokio::main]
async fn main() {
    println!("=== gate allows already-allowlisted device without reading any input ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &["node-known"],
        probe_path(),
    )));
    let mut lines = lines_from("");
    let result = ControlChannel::authenticate(&auth, "node-known", &mut lines).await;
    println!("result -> {result:?}");
    assert!(result.is_ok(), "known device must pass without needing a PIN");

    println!();
    println!("=== gate allows any device when PIN auth disabled ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(None, &[], probe_path())));
    let mut lines = lines_from("");
    let result = ControlChannel::authenticate(&auth, "node-totally-unknown", &mut lines).await;
    println!("result -> {result:?}");
    assert!(result.is_ok(), "auth disabled must let any device through");

    println!();
    println!("=== gate accepts unknown device with correct PIN, allowlists it ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut lines = lines_from("{\"type\":\"pin\",\"pin\":\"123456\"}\n");
    let result = ControlChannel::authenticate(&auth, "node-new", &mut lines).await;
    let now_allowed = auth.lock().unwrap().contains_key("node-new");
    println!("result -> {result:?}, now_allowlisted -> {now_allowed}");
    assert!(result.is_ok(), "correct PIN must be accepted");
    assert!(now_allowed, "device must be added to the allowlist after a correct PIN");

    println!();
    println!("=== gate rejects unknown device with wrong PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut lines = lines_from("{\"type\":\"pin\",\"pin\":\"000000\"}\n");
    let result = ControlChannel::authenticate(&auth, "node-attacker", &mut lines).await;
    let now_allowed = auth.lock().unwrap().contains_key("node-attacker");
    println!("result -> {result:?}, now_allowlisted -> {now_allowed}");
    assert_eq!(result, Err("incorrect PIN".to_string()));
    assert!(!now_allowed, "a wrong-PIN device must never be added to the allowlist");

    println!();
    println!("=== gate rejects unknown device sending a non-PIN message first ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut lines = lines_from("{\"type\":\"prompt\",\"text\":\"do something\"}\n");
    let result = ControlChannel::authenticate(&auth, "node-skipping-pin", &mut lines).await;
    println!("result -> {result:?}");
    assert!(result.is_err(), "a prompt sent before PIN auth must be rejected, not queued/processed");

    println!();
    println!("=== gate rejects unknown device that closes before sending PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut lines = lines_from(""); // EOF immediately
    let result = ControlChannel::authenticate(&auth, "node-ghost", &mut lines).await;
    println!("result -> {result:?}");
    assert_eq!(result, Err("connection closed before PIN was presented".to_string()));

    println!();
    println!("=== gate rejects unknown device sending malformed JSON as PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut lines = lines_from("not json at all\n");
    let result = ControlChannel::authenticate(&auth, "node-garbage", &mut lines).await;
    println!("result -> {result:?}");
    assert!(result.is_err());

    println!();
    println!("=== gate rejects unknown device sending empty PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut lines = lines_from("{\"type\":\"pin\",\"pin\":\"\"}\n");
    let result = ControlChannel::authenticate(&auth, "node-empty-pin", &mut lines).await;
    println!("result -> {result:?}");
    assert!(result.is_err(), "empty PIN must never satisfy verify_pin");

    println!();
    println!("auth_gate_probe: OK -- all authenticate() gate cases witnessed via real execution");
}
