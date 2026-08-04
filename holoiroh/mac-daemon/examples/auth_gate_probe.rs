//! Manual, run-by-hand probe: exercises the real `ControlChannel::authenticate` PIN/allowlist
//! gate directly, against a real in-memory `AuthState` (built via the real `AuthState`, not a
//! reimplementation) and a bounded `AsyncBufRead` reader over an in-memory byte buffer, printing
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

use std::io::Write;
use std::sync::{Arc, Mutex};

use holoiroh_daemon::control_channel::{
    AuthState, ControlChannel, MAX_AUTH_FRAME_BYTES, control_frame_digest,
};
use tracing_subscriber::fmt::MakeWriter;

const KNOWN_DEVICE: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const NEW_DEVICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const UNKNOWN_DEVICE: &str = "2222222222222222222222222222222222222222222222222222222222222222";

#[derive(Clone)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

fn reader_from(input: &str) -> std::io::Cursor<&[u8]> {
    std::io::Cursor::new(input.as_bytes())
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
    let captured_logs = LogCapture(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(captured_logs.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    assert_eq!(
        control_frame_digest(b"diagnostic-frame"),
        "09d4a4872842e6e028eafc5e"
    );

    println!("=== gate allows already-allowlisted device without reading any input ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[KNOWN_DEVICE],
        probe_path(),
    )));
    let mut reader = reader_from("");
    let result = ControlChannel::authenticate(&auth, KNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert!(
        result.is_ok(),
        "known device must pass without needing a PIN"
    );

    println!();
    println!("=== gate allows any device when PIN auth disabled ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        None,
        &[],
        probe_path(),
    )));
    let mut reader = reader_from("");
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert!(result.is_ok(), "auth disabled must let any device through");

    println!();
    println!("=== gate accepts unknown device with correct PIN, allowlists it ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = reader_from("{\"type\":\"pin\",\"pin\":\"123456\"}\n");
    let result = ControlChannel::authenticate(&auth, NEW_DEVICE, &mut reader).await;
    let now_allowed = auth.lock().unwrap().contains_key(NEW_DEVICE);
    println!("result -> {result:?}, now_allowlisted -> {now_allowed}");
    assert!(result.is_ok(), "correct PIN must be accepted");
    assert!(
        now_allowed,
        "device must be added to the allowlist after a correct PIN"
    );

    println!();
    println!("=== gate rejects unknown device with wrong PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = reader_from("{\"type\":\"pin\",\"pin\":\"000000\"}\n");
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    let now_allowed = auth.lock().unwrap().contains_key(UNKNOWN_DEVICE);
    println!("result -> {result:?}, now_allowlisted -> {now_allowed}");
    assert_eq!(result, Err("incorrect PIN".to_string()));
    assert!(
        !now_allowed,
        "a wrong-PIN device must never be added to the allowlist"
    );

    println!();
    println!("=== gate rejects unknown device sending a non-PIN message first ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = reader_from("{\"type\":\"prompt\",\"text\":\"do something\"}\n");
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert!(
        result.is_err(),
        "a prompt sent before PIN auth must be rejected, not queued/processed"
    );

    println!();
    println!("=== gate rejects unknown device that closes before sending PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = reader_from(""); // EOF immediately
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert_eq!(
        result,
        Err("connection closed before PIN was presented".to_string())
    );

    println!();
    println!("=== gate rejects unknown device sending malformed JSON as PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = reader_from("not json at all\n");
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert!(result.is_err());

    println!();
    println!("=== malformed structured input logs only safe request metadata ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let malformed_with_metadata = concat!(
        "{\"type\":\"transcribe_audio\",",
        "\"message_id\":\"msg-safe\",",
        "\"request_id\":\"req-safe\",",
        "\"audio_data_base64\":[\"RAW_AUDIO_SENTINEL\"],",
        "\"format\":\"wav\"}\n"
    );
    let mut reader = reader_from(malformed_with_metadata);
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert!(result.is_err());

    println!();
    println!("=== gate rejects invalid UTF-8 with digest-only diagnostics ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = std::io::Cursor::new(vec![0xff, b'\n']);
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert_eq!(result, Err("PIN frame is not valid UTF-8".to_string()));

    println!();
    println!("=== gate rejects over-limit PIN frame without unbounded allocation ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let oversized = format!("{}\n", "x".repeat(MAX_AUTH_FRAME_BYTES + 1));
    let mut reader = reader_from(&oversized);
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert_eq!(
        result,
        Err(format!(
            "PIN frame exceeds {MAX_AUTH_FRAME_BYTES}-byte limit"
        ))
    );

    println!();
    println!("=== gate rejects unknown device sending empty PIN ===");
    let auth = Arc::new(std::sync::Mutex::new(AuthState::for_probing(
        Some("123456"),
        &[],
        probe_path(),
    )));
    let mut reader = reader_from("{\"type\":\"pin\",\"pin\":\"\"}\n");
    let result = ControlChannel::authenticate(&auth, UNKNOWN_DEVICE, &mut reader).await;
    println!("result -> {result:?}");
    assert!(result.is_err(), "empty PIN must never satisfy verify_pin");

    let logs = String::from_utf8(captured_logs.0.lock().unwrap().clone()).unwrap();
    for raw_sentinel in [
        "do something",
        "not json at all",
        "RAW_AUDIO_SENTINEL",
        "000000",
        "123456",
    ] {
        assert!(
            !logs.contains(raw_sentinel),
            "raw control payload leaked into logs: {raw_sentinel}"
        );
    }
    assert!(logs.contains("message_id=msg-safe") || logs.contains("message_id=\"msg-safe\""));
    assert!(logs.contains("request_id=req-safe") || logs.contains("request_id=\"req-safe\""));
    let digests: Vec<&str> = logs
        .split_whitespace()
        .filter_map(|field| field.strip_prefix("frame_digest="))
        .map(|value| value.trim_matches('"'))
        .collect();
    assert_eq!(
        digests.len(),
        4,
        "only malformed, invalid-UTF8, and over-limit diagnostics should carry frame digests"
    );
    assert!(digests.iter().all(|digest| {
        digest.len() == 24 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }));

    println!();
    println!(
        "auth_gate_probe: OK -- bounded auth, safe metadata, and digest-only diagnostics witnessed"
    );
}
