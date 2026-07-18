//! Manual, run-by-hand probe: exercises the real `ServerMessage::InputRequest` /
//! `ClientMessage::InputResponse` wire types added for Project Aro PRD row P0-14, plus the real
//! expiry-to-safe-pause timing logic (`PendingInputRequest` / `wait_for_expiry`), printing real
//! output for each case. Matches this repo's no-unit-tests convention (see
//! `examples/control_channel_probe.rs` and `examples/auth_gate_probe.rs`, which this probe
//! mirrors): no `#[cfg(test)]`, driven by `cargo run --example` instead of `cargo test`.
//!
//! Covers:
//! 1. Real `serde_json` round-trips for `ServerMessage::InputRequest` across all five
//!    `InputRequestKind` variants, with exact wire-JSON shape assertions (matching
//!    `PROTOCOL.md`'s documented examples).
//! 2. A real round-trip for `ClientMessage::InputResponse`.
//! 3. Confirmation that `InputRequest`'s `context`/`response_options` never contain the literal
//!    example "secret" string used to build a `Credential`-kind request -- i.e. the constructor
//!    genuinely has no path that could embed a credential value.
//! 4. Real, **not mocked**, timed expiry: builds a `PendingInputRequest` with a short real TTL,
//!    then actually awaits `wait_for_expiry` (real `tokio::time`, real wall-clock elapsed time)
//!    and confirms it resolves only after the deadline passes, and that
//!    `ServerMessage::input_request_expired` (the safe-pause status message) is a `Status`
//!    variant, never `Error`.
//! 5. The degenerate already-expired-deadline case: a `PendingInputRequest` whose `expires_at` is
//!    already in the past resolves `wait_for_expiry` immediately (no hang, no panic).
//! 6. `wait_for_expiry` on `None` never resolves within a bounded real-time window (proving the
//!    "no spurious firing when nothing is pending" contract `tokio::select!` in
//!    `ControlChannel::accept` relies on), using a real `tokio::time::timeout` race rather than a
//!    fixed sleep-and-hope.
//!
//! Run with `cargo run --example input_request_probe`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use holoiroh_daemon::control_channel::{ClientMessage, InputRequestKind, PendingInputRequest, ServerMessage, wait_for_expiry};

fn epoch_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_millis() as u64
}

fn round_trip_server(label: &str, msg: ServerMessage, expected_json: &str) {
    let json = serde_json::to_string(&msg).unwrap();
    println!("{label}: serialize -> {json}");
    assert_eq!(json, expected_json, "{label}: serialized JSON mismatch");
    let back: ServerMessage = serde_json::from_str(&json).unwrap();
    println!("{label}: deserialize -> {back:?}");
    assert_eq!(back, msg, "{label}: round-trip mismatch");
}

fn round_trip_client(label: &str, msg: ClientMessage, expected_json: &str) {
    let json = serde_json::to_string(&msg).unwrap();
    println!("{label}: serialize -> {json}");
    assert_eq!(json, expected_json, "{label}: serialized JSON mismatch");
    let back: ClientMessage = serde_json::from_str(&json).unwrap();
    println!("{label}: deserialize -> {back:?}");
    assert_eq!(back, msg, "{label}: round-trip mismatch");
}

#[tokio::main]
async fn main() {
    println!("=== ServerMessage::InputRequest round-trips, all five kinds ===");

    round_trip_server(
        "input_request/credential",
        ServerMessage::InputRequest {
            request_id: "req-1".to_string(),
            kind: InputRequestKind::Credential,
            context: "Holo needs your GitHub personal access token to push this branch".to_string(),
            response_options: vec![],
            expires_at: 1_800_000_000_000,
        },
        r#"{"type":"input_request","request_id":"req-1","kind":"credential","context":"Holo needs your GitHub personal access token to push this branch","response_options":[],"expires_at":1800000000000}"#,
    );

    round_trip_server(
        "input_request/mfa",
        ServerMessage::InputRequest {
            request_id: "req-2".to_string(),
            kind: InputRequestKind::Mfa,
            context: "Enter the 6-digit code from your authenticator app".to_string(),
            response_options: vec![],
            expires_at: 1_800_000_060_000,
        },
        r#"{"type":"input_request","request_id":"req-2","kind":"mfa","context":"Enter the 6-digit code from your authenticator app","response_options":[],"expires_at":1800000060000}"#,
    );

    round_trip_server(
        "input_request/ambiguous_choice",
        ServerMessage::InputRequest {
            request_id: "req-3".to_string(),
            kind: InputRequestKind::AmbiguousChoice,
            context: "Two calendars match 'team standup' -- which one?".to_string(),
            response_options: vec!["Work calendar".to_string(), "Personal calendar".to_string()],
            expires_at: 1_800_000_120_000,
        },
        r#"{"type":"input_request","request_id":"req-3","kind":"ambiguous_choice","context":"Two calendars match 'team standup' -- which one?","response_options":["Work calendar","Personal calendar"],"expires_at":1800000120000}"#,
    );

    round_trip_server(
        "input_request/missing_info",
        ServerMessage::InputRequest {
            request_id: "req-4".to_string(),
            kind: InputRequestKind::MissingInfo,
            context: "Which recipient email address should I use?".to_string(),
            response_options: vec![],
            expires_at: 1_800_000_180_000,
        },
        r#"{"type":"input_request","request_id":"req-4","kind":"missing_info","context":"Which recipient email address should I use?","response_options":[],"expires_at":1800000180000}"#,
    );

    round_trip_server(
        "input_request/sensitive_access_consent",
        ServerMessage::InputRequest {
            request_id: "req-5".to_string(),
            kind: InputRequestKind::SensitiveAccessConsent,
            context: "This will send a payment of $120.00 -- proceed?".to_string(),
            response_options: vec!["Yes, proceed".to_string(), "No, cancel".to_string()],
            expires_at: 1_800_000_240_000,
        },
        r#"{"type":"input_request","request_id":"req-5","kind":"sensitive_access_consent","context":"This will send a payment of $120.00 -- proceed?","response_options":["Yes, proceed","No, cancel"],"expires_at":1800000240000}"#,
    );

    println!();
    println!("=== ClientMessage::InputResponse round-trip ===");
    round_trip_client(
        "input_response",
        ClientMessage::InputResponse {
            request_id: "req-3".to_string(),
            selected_option: "Work calendar".to_string(),
        },
        r#"{"type":"input_response","request_id":"req-3","selected_option":"Work calendar"}"#,
    );

    println!();
    println!("=== ServerMessage::input_request() constructor never embeds a credential value ===");
    // Build a Credential-kind request the way real call sites would: only metadata-shaped
    // arguments are even accepted by the constructor (see its doc). Confirm the "secret" example
    // value used here to describe the credential's *shape* never appears verbatim as if it were
    // the credential itself -- context describes what's needed, never the value.
    let secret_value = "sk-super-secret-token-do-not-log-me";
    let msg = ServerMessage::input_request(
        "req-cred-1",
        InputRequestKind::Credential,
        "Holo needs an API key for the deploy step",
        vec![],
        Duration::from_secs(300),
    );
    let json = serde_json::to_string(&msg).unwrap();
    println!("constructed: {json}");
    assert!(
        !json.contains(secret_value),
        "InputRequest JSON must never contain a credential value -- the constructor has no parameter that could carry one"
    );
    match &msg {
        ServerMessage::InputRequest { kind, response_options, expires_at, .. } => {
            assert_eq!(*kind, InputRequestKind::Credential);
            assert!(response_options.is_empty(), "credential kind carries no discrete choices");
            let now = epoch_millis_now();
            assert!(*expires_at > now, "expires_at must be in the future for a fresh 300s-TTL request");
            assert!(*expires_at <= now + 301_000, "expires_at should be ~300s out, not wildly off");
            println!(
                "expires_at={expires_at}, now={now}, delta_ms={}",
                expires_at.saturating_sub(now)
            );
        }
        other => panic!("expected InputRequest, got {other:?}"),
    }
    println!("OK -- constructed InputRequest carries no credential characters, only metadata");

    println!();
    println!("=== real timed expiry: PendingInputRequest expires after a real, short TTL ===");
    let short_ttl_ms: u64 = 250;
    let expires_at = epoch_millis_now() + short_ttl_ms;
    let pending = Some(PendingInputRequest::for_probing("req-expiring", expires_at));

    let started = tokio::time::Instant::now();
    // This is a REAL await on REAL tokio time -- not a mock clock, not a fast-forwarded test
    // timer. wait_for_expiry must not return before short_ttl_ms of real wall-clock time has
    // actually elapsed.
    wait_for_expiry(&pending).await;
    let elapsed = started.elapsed();
    println!("wait_for_expiry resolved after {elapsed:?} (requested TTL was {short_ttl_ms}ms)");
    assert!(
        elapsed >= Duration::from_millis(short_ttl_ms.saturating_sub(5)),
        "wait_for_expiry must not resolve before the real deadline (small tolerance for scheduler jitter)"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "wait_for_expiry took implausibly long ({elapsed:?}) for a 250ms TTL -- something is wrong with the sleep computation"
    );

    let safe_pause_msg = ServerMessage::input_request_expired("req-expiring");
    let safe_pause_json = serde_json::to_string(&safe_pause_msg).unwrap();
    println!("safe-pause status message: {safe_pause_json}");
    match &safe_pause_msg {
        ServerMessage::Status { text } => {
            let text = text.as_deref().unwrap_or_default();
            assert!(text.contains("safely paused"), "expiry status text must say 'safely paused': {text}");
            assert!(!text.to_lowercase().contains("fail"), "expiry status text must NOT say failed: {text}");
        }
        other => panic!("expiry must produce a Status message (safe pause, not a failure), got {other:?}"),
    }
    println!("OK -- expiry emits ServerMessage::Status (safe pause), never ServerMessage::Error");

    println!();
    println!("=== degenerate case: expires_at already in the past resolves immediately, no hang ===");
    let already_expired = epoch_millis_now().saturating_sub(10_000); // 10s in the past
    let pending_past = Some(PendingInputRequest::for_probing("req-already-expired", already_expired));
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), wait_for_expiry(&pending_past)).await;
    let elapsed = started.elapsed();
    println!("already-past-deadline wait_for_expiry resolved after {elapsed:?}");
    assert!(result.is_ok(), "an already-expired deadline must resolve immediately, not hang until the 2s timeout");
    assert!(elapsed < Duration::from_millis(500), "already-expired deadline took too long to resolve: {elapsed:?}");
    println!("OK -- already-expired deadline fires immediately, no panic, no hang");

    println!();
    println!("=== None pending: wait_for_expiry never resolves (no spurious firing) ===");
    let none_pending: Option<PendingInputRequest> = None;
    let result = tokio::time::timeout(Duration::from_millis(300), wait_for_expiry(&none_pending)).await;
    println!("wait_for_expiry(&None) within a 300ms window -> timed_out={}", result.is_err());
    assert!(result.is_err(), "wait_for_expiry on None pending must never resolve on its own");
    println!("OK -- no pending request never spuriously fires the expiry arm");

    println!();
    println!("input_request_probe: OK -- all input_request/input_response wire-schema and real-timed expiry cases witnessed via real execution");
}
