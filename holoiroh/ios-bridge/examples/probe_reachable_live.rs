//! Live, run-by-hand witness for the ADDITIVE reachability FFI
//! (`holoiroh_ios_bridge_probe_reachable`), per this repo's no-unit-tests rule:
//! it drives the real `extern "C"` function on the host triple against a *real*
//! running `holoiroh-daemon` -- the exact call the Swift `ReachabilityMonitor`
//! makes, minus Swift.
//!
//! This is a LOCAL live probe, deliberately NOT wired into CI: reachability
//! depends on a live daemon on the same network, which a headless CI runner has
//! no way to stand up deterministically.
//!
//! Usage:
//! 1. Start the daemon on the Mac (`cargo run -p holoiroh-daemon`).
//! 2. `cargo run -p holoiroh-ios-bridge --example probe_reachable_live`
//!    (optionally pass a ticket as the first arg to override the default).
//!
//! Expected output with the daemon up:
//!   default daemon ticket -> reachable=true
//!   dead node ticket      -> reachable=false
//!   malformed ticket      -> reachable=false
//!   PROBE OK

use std::ffi::CString;

use holoiroh_ios_bridge::holoiroh_ios_bridge_probe_reachable;

const DEFAULT_DAEMON_TICKET: &str =
    "iroh-live:nhWuOUavJaTyFA2AXzWPTiUUg38hFs6cOjKHKJu9pXwA/holoiroh";

const DEAD_NODE_TICKET: &str =
    "iroh-live:AAAAAAAAJaTyFA2AXzWPTiUUg38hFs6cOjKHKJu9pXwA/holoiroh";

const MALFORMED_TICKET: &str = "not-a-valid-ticket";

fn probe(label: &str, ticket: &str, timeout_ms: u64) -> bool {
    let c = CString::new(ticket).expect("ticket has no interior NUL");
    let reachable = unsafe { holoiroh_ios_bridge_probe_reachable(c.as_ptr(), timeout_ms) };
    println!("{label:<22} -> reachable={reachable}  ({ticket})");
    reachable
}

fn main() {
    let daemon_ticket = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_DAEMON_TICKET.to_string());

    let daemon = probe("default daemon ticket", &daemon_ticket, 8000);
    let dead = probe("dead node ticket", DEAD_NODE_TICKET, 6000);
    let malformed = probe("malformed ticket", MALFORMED_TICKET, 2000);

    assert!(!dead, "a well-formed ticket to a nonexistent node must be unreachable");
    assert!(!malformed, "a malformed ticket must be unreachable (parse failure)");

    if daemon {
        println!("PROBE OK: daemon reachable=true; dead + malformed reachable=false");
    } else {
        println!(
            "PROBE PARTIAL: daemon reachable=false -- is holoiroh-daemon running? \
             (dead + malformed correctly false)"
        );
        std::process::exit(3);
    }
}
