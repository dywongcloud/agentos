//! Real-execution probe for the `holoiroh-ios-bridge` `extern "C"` surface.
//!
//! This repo has a NO-UNIT-TESTS rule: validation is a `cargo run --example`
//! binary that exercises the real code path and reads its actual output, not a
//! `#[cfg(test)]`/`*.test` file asserted later. This probe drives the C-ABI
//! exactly as the Swift side (via the generated header) would, and witnesses:
//!
//!   1. `holoiroh_ios_bridge_new` returns a non-null opaque handle (real Tokio
//!      runtime + iroh `Endpoint` bind + `Live` session spawn).
//!   2. `ticket_connect` with a **malformed** ticket returns
//!      `HOLOIROH_ERR_INVALID_TICKET` + a heap error string (freed cleanly),
//!      never a panic.
//!   3. `ticket_connect` with a **well-formed but unreachable** ticket returns
//!      a clean negative status (the real network dial cannot complete
//!      headlessly -- the point is that it fails cleanly, not that it
//!      connects) + a heap error string, never a panic/hang beyond a bounded
//!      attempt.
//!   4. `subscribe` on a **not-connected** bridge returns null + an error
//!      string, never a panic.
//!   5. `poll_next_frame` / `subscribe` / `free` tolerate **null** arguments
//!      (the `free(NULL)` C convention + null-arg guards).
//!   6. `control_send` / `poll_control_event` on a bridge that never called
//!      `control_connect` return `HOLOIROH_ERR_NOT_CONNECTED`, never a panic
//!      (the live control-channel handshake/send/receive path is witnessed
//!      separately by `examples/control_ffi_probe.rs` against a real daemon).
//!   7. Full teardown (`subscription_free`, `free`) runs with no crash/leak.
//!
//! What this probe CANNOT witness headlessly: a real frame actually arriving.
//! That needs a live publisher (the Mac daemon) reachable over a real network
//! + relay, which this sandbox's iroh relay infrastructure does not provide.
//! The C-ABI contract, the error paths, and clean construction/teardown ARE
//! witnessable here, and that is exactly what this probe proves.

use std::ffi::{CStr, CString, c_char};
use std::ptr;

use holoiroh_ios_bridge::{
    HOLOIROH_ERR_INVALID_TICKET, HOLOIROH_ERR_NOT_CONNECTED, HOLOIROH_OK, HoloirohFrame,
    holoiroh_ios_bridge_control_send, holoiroh_ios_bridge_free,
    holoiroh_ios_bridge_free_error_string, holoiroh_ios_bridge_new,
    holoiroh_ios_bridge_poll_control_event, holoiroh_ios_bridge_poll_next_frame,
    holoiroh_ios_bridge_subscribe, holoiroh_ios_bridge_subscription_free,
    holoiroh_ios_bridge_ticket_connect,
};

/// Take ownership of (and free) a Rust-allocated error string handed back via
/// an `out_error` out-param, returning its text for logging. Frees through the
/// crate's own free fn (matching allocator), so this both reads AND witnesses
/// the free path is crash-free.
fn take_error(err_ptr: *mut c_char) -> Option<String> {
    if err_ptr.is_null() {
        return None;
    }
    // SAFETY: err_ptr came from this crate's set_error (CString::into_raw).
    let text = unsafe { CStr::from_ptr(err_ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { holoiroh_ios_bridge_free_error_string(err_ptr) };
    Some(text)
}

fn main() {
    println!("=== holoiroh-ios-bridge ffi_probe: exercising the extern \"C\" surface ===\n");

    // --- 1. Construct the bridge (real runtime + endpoint bind + Live spawn) ---
    println!("[1] holoiroh_ios_bridge_new()");
    let bridge = unsafe { holoiroh_ios_bridge_new() };
    assert!(!bridge.is_null(), "bridge_new returned null (construction failed)");
    println!("    -> non-null bridge handle ({bridge:p}): runtime + iroh Endpoint + Live session up\n");

    // --- 2. ticket_connect with a MALFORMED ticket -> INVALID_TICKET, clean error ---
    println!("[2] ticket_connect(\"not-a-real-ticket\")  (malformed)");
    {
        let bad = CString::new("not-a-real-ticket").unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let status =
            unsafe { holoiroh_ios_bridge_ticket_connect(bridge, bad.as_ptr(), &mut err) };
        let msg = take_error(err);
        println!("    -> status={status} (expected HOLOIROH_ERR_INVALID_TICKET={HOLOIROH_ERR_INVALID_TICKET})");
        println!("    -> error string: {msg:?}");
        assert_eq!(
            status, HOLOIROH_ERR_INVALID_TICKET,
            "malformed ticket must yield HOLOIROH_ERR_INVALID_TICKET, not {status}"
        );
        assert!(msg.is_some(), "a malformed ticket must produce an error string");
        println!("    OK: malformed ticket rejected cleanly (no panic), error string freed\n");
    }

    // --- 3. ticket_connect with a WELL-FORMED but UNREACHABLE ticket ---
    //   A real iroh-live ticket for a node that isn't actually up here. The
    //   parse succeeds (valid iroh-live: URI), the dial cannot complete in
    //   this sandbox -> a clean negative status, never a panic. We assert only
    //   that it returns cleanly (either CONNECT_FAILED after the dial gives
    //   up, or some negative status), and never OK, and never panics.
    println!("[3] ticket_connect(<well-formed, unreachable iroh-live: ticket>)");
    {
        // A syntactically valid iroh-live ticket (from the vendored crate's
        // own round-trip format: iroh-live:<base64url(postcard(EndpointAddr))>/<name>).
        // This node id is random/offline, so the dial has nowhere to land.
        let ticket_str = sample_unreachable_ticket();
        println!("    ticket = {ticket_str}");
        let ticket = CString::new(ticket_str).unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let status =
            unsafe { holoiroh_ios_bridge_ticket_connect(bridge, ticket.as_ptr(), &mut err) };
        let msg = take_error(err);
        println!("    -> status={status} (expected a negative connect failure, NOT {HOLOIROH_OK})");
        println!("    -> error string: {msg:?}");
        assert_ne!(
            status, HOLOIROH_OK,
            "an unreachable ticket must not report success"
        );
        assert!(status < 0, "failure must be a negative status, got {status}");
        println!("    OK: unreachable ticket failed cleanly (no panic), status negative\n");
    }

    // --- 4. subscribe on a not-connected bridge -> null + error ---
    //   (Connect above did not succeed, so no subscription is stored.)
    println!("[4] subscribe(bridge)  (no successful connect -> not connected)");
    {
        let mut err: *mut c_char = ptr::null_mut();
        let sub = unsafe { holoiroh_ios_bridge_subscribe(bridge, &mut err) };
        let msg = take_error(err);
        println!("    -> subscription ptr = {sub:p} (expected null)");
        println!("    -> error string: {msg:?}");
        assert!(sub.is_null(), "subscribe on a not-connected bridge must return null");
        assert!(msg.is_some(), "not-connected subscribe must produce an error string");
        println!("    OK: not-connected subscribe returned null cleanly\n");
    }

    // --- 5. null-argument tolerance across the surface ---
    println!("[5] null-argument tolerance");
    {
        // poll on null subscription -> negative status, no crash.
        let mut frame = HoloirohFrame {
            width: 0,
            height: 0,
            timestamp_us: 0,
            pixel_format: 0,
            kind: 0,
        };
        let mut buf = [0u8; 16];
        let poll_null =
            unsafe { holoiroh_ios_bridge_poll_next_frame(ptr::null_mut(), buf.as_mut_ptr(), buf.len(), &mut frame) };
        println!("    poll_next_frame(null sub) -> {poll_null} (negative, no crash)");
        assert!(poll_null < 0, "poll on null subscription must be a negative status");

        // subscribe(null bridge) -> null, no crash.
        let mut err: *mut c_char = ptr::null_mut();
        let sub_null = unsafe { holoiroh_ios_bridge_subscribe(ptr::null_mut(), &mut err) };
        let _ = take_error(err);
        assert!(sub_null.is_null(), "subscribe(null) must return null");
        println!("    subscribe(null bridge) -> null (no crash)");

        // free(NULL) / subscription_free(NULL) / free_error_string(NULL) are no-ops.
        unsafe { holoiroh_ios_bridge_free(ptr::null_mut()) };
        unsafe { holoiroh_ios_bridge_subscription_free(ptr::null_mut()) };
        unsafe { holoiroh_ios_bridge_free_error_string(ptr::null_mut()) };
        println!("    free(NULL) / subscription_free(NULL) / free_error_string(NULL) -> no-op, no crash");
        println!("    OK: null args tolerated across the surface\n");
    }

    // --- 6. control channel: honest UNSUPPORTED, never a panic ---
    println!("[6] control_send / poll_control_event  (honest unsupported)");
    {
        // The control channel is now REALLY implemented (holoiroh_ios_bridge_control_connect /
        // _control_send / _poll_control_event -- see examples/control_ffi_probe.rs for the full
        // live PIN-handshake + send/receive witness against a real daemon). This probe doesn't
        // dial a daemon, so it exercises the honest not-yet-connected error path instead: every
        // control fn on a bridge that never called control_connect must report
        // HOLOIROH_ERR_NOT_CONNECTED, never panic, never silently succeed.
        let json = CString::new(r#"{"type":"prompt","text":"hi"}"#).unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let send_status =
            unsafe { holoiroh_ios_bridge_control_send(bridge, json.as_ptr(), &mut err) };
        let send_msg = take_error(err);
        println!("    control_send (no control_connect yet) -> {send_status} (expected HOLOIROH_ERR_NOT_CONNECTED={HOLOIROH_ERR_NOT_CONNECTED})");
        println!("    control_send error string: {send_msg:?}");
        assert_eq!(send_status, HOLOIROH_ERR_NOT_CONNECTED);

        let mut out_json: *mut c_char = ptr::null_mut();
        let mut poll_err: *mut c_char = ptr::null_mut();
        let poll_status = unsafe {
            holoiroh_ios_bridge_poll_control_event(bridge, &mut out_json, &mut poll_err)
        };
        let poll_msg = take_error(poll_err);
        println!("    poll_control_event (no control_connect yet) -> {poll_status} (out_json null? {})", out_json.is_null());
        println!("    poll_control_event error string: {poll_msg:?}");
        assert_eq!(poll_status, HOLOIROH_ERR_NOT_CONNECTED);
        assert!(out_json.is_null(), "poll_control_event must null out_json when not connected");
        println!("    OK: control channel reports NOT_CONNECTED cleanly before control_connect (no panic)\n");
    }

    // --- 7. teardown: free the bridge (runs live.shutdown().await) ---
    println!("[7] holoiroh_ios_bridge_free(bridge)");
    unsafe { holoiroh_ios_bridge_free(bridge) };
    println!("    -> freed: subscription dropped, Live session shut down, runtime dropped\n");

    println!("ffi_probe: OK -- extern \"C\" construction, error paths, null-tolerance, and teardown all witnessed via real execution");
}

/// A syntactically valid `iroh-live:` ticket pointing at an offline/random
/// node, so `LiveTicket::from_str` parses it but the dial has nowhere to land.
///
/// Rather than hand-craft the base64url(postcard(EndpointAddr)) payload by
/// hand (fragile against upstream `EndpointAddr` layout changes), we build one
/// through the same public API the daemon uses: a fresh random `SecretKey` ->
/// `EndpointAddr` -> `LiveTicket::new(..).to_string()`. This is a real ticket
/// in the exact wire format the Mac daemon prints; it just describes a peer
/// that is not actually reachable from this probe.
fn sample_unreachable_ticket() -> String {
    use iroh::{EndpointAddr, SecretKey};
    use iroh_live::ticket::LiveTicket;

    // iroh 1.0.2's `SecretKey::generate()` takes no argument (matches
    // iroh-live's own `util.rs:17` / `ticket.rs:129` usage).
    let key = SecretKey::generate();
    let addr = EndpointAddr::from(key.public());
    LiveTicket::new(addr, "holoiroh").to_string()
}
