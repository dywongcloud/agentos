//! Live, run-by-hand witness for the control-channel FFI surface
//! (`holoiroh_ios_bridge_control_connect` / `_control_send` /
//! `_poll_control_event`), per this repo's no-unit-tests rule: it drives the
//! real `extern "C"` functions on the host triple against a *real* running
//! `holoiroh-daemon` -- exactly what the Swift side will do, minus Swift.
//!
//! Usage:
//! 1. Start the daemon on the Mac (`cargo run -p holoiroh-daemon`), note the
//!    `iroh-live:` ticket and (if auth is enabled) the pairing PIN it prints.
//! 2. `cargo run -p holoiroh-ios-bridge --example control_ffi_probe -- \
//!        '<iroh-live:ticket>' '<pin>'`
//!
//! Expected output: `ticket stored` (or a media-subscribe warning if the Mac
//! isn't broadcasting -- the control channel works regardless),
//! `control_connect OK`, an idempotency confirmation, then the daemon's
//! envelope lines (ack / task_progress / status / error) printed as they
//! arrive for ~10s after sending one envelope-wrapped `prompt`.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use holoiroh_ios_bridge::{
    HOLOIROH_ERR_ENDED, HOLOIROH_OK, holoiroh_ios_bridge_control_connect,
    holoiroh_ios_bridge_control_send, holoiroh_ios_bridge_free,
    holoiroh_ios_bridge_free_error_string, holoiroh_ios_bridge_new,
    holoiroh_ios_bridge_poll_control_event, holoiroh_ios_bridge_ticket_connect,
};

/// Takes ownership of an `out_error` string (freeing it via the bridge's own
/// free function) and returns its contents for printing.
fn take_err(err: *mut c_char) -> String {
    if err.is_null() {
        return "(no detail)".to_string();
    }
    let s = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
    unsafe { holoiroh_ios_bridge_free_error_string(err) };
    s
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: control_ffi_probe <iroh-live:ticket> <pin>";
    let ticket = args.next().expect(usage);
    let pin = args.next().expect(usage);

    unsafe {
        let bridge = holoiroh_ios_bridge_new();
        assert!(!bridge.is_null(), "holoiroh_ios_bridge_new returned null");

        // ticket_connect stores the peer address for the control channel as
        // soon as the ticket parses; a media-subscribe failure (Mac not
        // broadcasting) is tolerated here since it must not block control.
        let ticket_c = CString::new(ticket).expect("ticket contains NUL");
        let mut err: *mut c_char = std::ptr::null_mut();
        let status = holoiroh_ios_bridge_ticket_connect(bridge, ticket_c.as_ptr(), &mut err);
        if status == HOLOIROH_OK {
            println!("ticket stored (media subscribe OK)");
        } else {
            println!(
                "ticket stored (media subscribe failed, continuing -- control channel is \
                 independent): {}",
                take_err(err)
            );
        }

        // The function under test: PIN handshake + greeting.
        let pin_c = CString::new(pin).expect("pin contains NUL");
        let mut err: *mut c_char = std::ptr::null_mut();
        let status = holoiroh_ios_bridge_control_connect(bridge, pin_c.as_ptr(), &mut err);
        assert_eq!(
            status,
            HOLOIROH_OK,
            "control_connect failed: {}",
            take_err(err)
        );
        println!("control_connect OK (daemon greeting received)");

        // Idempotency witness: second call must be an immediate OK.
        let mut err: *mut c_char = std::ptr::null_mut();
        let status = holoiroh_ios_bridge_control_connect(bridge, pin_c.as_ptr(), &mut err);
        assert_eq!(status, HOLOIROH_OK, "idempotent reconnect: {}", take_err(err));
        println!("control_connect idempotent OK");

        // One envelope-wrapped prompt, built the same way the Swift client
        // will (TaskEnvelope<ClientMessage>, PROTOCOL.md). session_id is
        // client-chosen; the daemon validates expiry/dedup/sequence only.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_millis() as u64;
        let envelope = serde_json::json!({
            "protocol_version": 1,
            "message_id": format!("probe-{now_ms}"),
            "session_id": "control-ffi-probe",
            "message_type": "prompt",
            "sent_at": now_ms,
            "expires_at": now_ms + 30_000,
            "sequence_number": 0,
            "payload": { "type": "prompt", "text": "say hello from the control ffi probe" },
        })
        .to_string();
        let envelope_c = CString::new(envelope).expect("envelope contains NUL");
        let mut err: *mut c_char = std::ptr::null_mut();
        let status = holoiroh_ios_bridge_control_send(bridge, envelope_c.as_ptr(), &mut err);
        assert_eq!(status, HOLOIROH_OK, "control_send failed: {}", take_err(err));
        println!("control_send OK, polling for events for ~10s...");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            let mut out_json: *mut c_char = std::ptr::null_mut();
            let mut err: *mut c_char = std::ptr::null_mut();
            let status =
                holoiroh_ios_bridge_poll_control_event(bridge, &mut out_json, &mut err);
            if status == HOLOIROH_ERR_ENDED {
                println!("control channel ended by daemon");
                break;
            }
            assert_eq!(status, HOLOIROH_OK, "poll failed: {}", take_err(err));
            if out_json.is_null() {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            let line = CStr::from_ptr(out_json).to_string_lossy().into_owned();
            holoiroh_ios_bridge_free_error_string(out_json);
            println!("event: {line}");
        }

        holoiroh_ios_bridge_free(bridge);
        println!("bridge freed cleanly -- probe complete");
    }
}
