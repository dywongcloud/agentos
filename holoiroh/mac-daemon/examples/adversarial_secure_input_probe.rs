//! Adversarial VERIFY-phase witness for `ServerMessage::SecureInputState`
//! decode robustness: missing field, wrong type, null, empty string, and an
//! empty object must all decode as `Err`, never panic. Not a permanent test
//! file -- a one-shot corner-case witness for this chain's VERIFY sweep, run
//! once and left as a durable regression probe alongside its siblings.

use holoiroh_daemon::control_channel::ServerMessage;

fn main() {
    let cases: [&str; 6] = [
        r#"{"type":"secure_input_state"}"#,
        r#"{"type":"secure_input_state","active":"yes"}"#,
        r#"{"type":"secure_input_state","active":null}"#,
        r#"{"type":"secure_input_state","active":1}"#,
        "",
        "{}",
    ];
    for c in cases {
        let r: Result<ServerMessage, _> = serde_json::from_str(c);
        println!("input={c:?} -> ok={}", r.is_ok());
        assert!(r.is_err(), "malformed input unexpectedly decoded: {c:?}");
    }
    println!(
        "adversarial_secure_input_probe: OK -- every malformed/degenerate input decoded as Err, no panic"
    );
}
