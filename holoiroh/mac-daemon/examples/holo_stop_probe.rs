//! Manual, run-by-hand probe for the **remote kill-switch** path: an iOS `Stop` control sends a
//! `ClientMessage::Stop` over the control channel, which the daemon maps to an internal
//! `ControlMessage::Stop` and, for a stop with no `context_id` (exactly what the wire schema
//! produces), engages the global `holo stop` CLI kill switch.
//!
//! This witnesses the three things the kill-switch row asks for, via real execution (NO test
//! file, per this repo's no-unit-tests rule -- run with
//! `cargo run --example holo_stop_probe`):
//!
//!   1. **Wire -> internal mapping.** `control_channel::to_control_message(rid,
//!      ClientMessage::Stop)` produces exactly `ControlMessage::Stop { request_id: rid,
//!      context_id: None, force: false }` -- the exact `ClientMessage::Stop` ->
//!      `ControlMessage::Stop` translation the accept loop performs before handing the message to
//!      `HoloControlBridge`. `context_id: None` is the load-bearing part: it is what makes
//!      `handle_stop` take the *global* `holo stop` branch rather than a scoped A2A cancel.
//!
//!   2. **`holo stop` invocation construction.** `stop::build_stop_args(force)` -- the exact
//!      argument vector `holo_stop` shells to the `holo` CLI -- is `["stop"]` for a graceful
//!      pause-then-cancel and `["stop", "--force"]` when force is requested. Asserting on the
//!      pure builder (rather than only the running command) lets the `--force` shape be witnessed
//!      without actually SIGKILLing a `hai-agent-runtime` process.
//!
//!   3. **Real `holo stop` fires.** Unlike `holo_bridge_queue_probe` (written for a sandbox with
//!      no `holo` on PATH), `holo-desktop-cli` *is* installed on this machine at
//!      `~/.holo/bin/holo`. When that binary is present, this probe actually invokes
//!      `stop::holo_stop(<real holo>, force=false)` and confirms it returns `Ok(())` -- a real
//!      `holo stop` run. With no Holo turn in flight this is a benign no-op (it writes the
//!      `~/.holo/stop` timestamp file, which only affects turns started *after* the request --
//!      see `stop.rs`'s module doc), so it is safe to run here. If `~/.holo/bin/holo` is not
//!      found, this step is skipped with a printed note rather than faked.
//!
//! Plus the end-to-end bridge behavior and the failure mode:
//!
//!   4. **`HoloControlBridge::handle(ControlMessage::Stop { context_id: None })`** against an
//!      unreachable A2A endpoint, with a prompt queued behind an in-flight one, emits an `Ack`
//!      and a terminal `Done { Canceled }` for the dropped queued prompt and leaves the queue
//!      empty -- the same stop-drains-the-queue behavior `holo_bridge_queue_probe` witnesses,
//!      re-witnessed here from the kill-switch's own entry point. (The real `holo stop` sub-call
//!      inside `handle_stop` runs against whatever `holo_bin` the bridge was built with; this
//!      case uses the real binary when available so the full `handle_stop` body -- queue drain
//!      *and* `holo stop` -- executes, not a partial path.)
//!
//!   5. **Graceful failure.** A `HoloControlBridge` built with a bogus `holo_bin` path, handed a
//!      `Stop { context_id: None }`, surfaces a `ControlEvent::Error` ("holo stop failed: ...")
//!      rather than panicking -- the kill-switch degrades cleanly if the `holo` CLI is missing.

use std::path::PathBuf;
use std::time::Duration;

use holoiroh_daemon::control_channel::{ClientMessage, to_control_message};
use holoiroh_daemon::holo_bridge::a2a_client::A2aClient;
use holoiroh_daemon::holo_bridge::control::DoneStatus;
use holoiroh_daemon::holo_bridge::stop::{build_stop_args, holo_stop};
use holoiroh_daemon::holo_bridge::{ControlEvent, ControlMessage, HoloControlBridge};
use tokio::sync::mpsc;

/// Resolves `~/.holo/bin/holo` if it exists, so the probe can invoke the real kill-switch binary
/// when present and skip (rather than fake) that step when it isn't.
fn real_holo_bin() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".holo/bin/holo");
    if path.exists() { Some(path) } else { None }
}

fn unreachable_bridge(
    holo_bin: &str,
) -> (HoloControlBridge, mpsc::UnboundedReceiver<ControlEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    // RFC 5737 TEST-NET-1, guaranteed not to route: `send_and_stream` fails fast so a prompt turn
    // stays "in flight" only as long as its connect timeout, deterministically.
    let client = A2aClient::new("http://192.0.2.1:1".to_string(), "probe-token".to_string());
    (HoloControlBridge::new(client, holo_bin, tx), rx)
}

fn drain(rx: &mut mpsc::UnboundedReceiver<ControlEvent>) -> Vec<ControlEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::main]
async fn main() {
    println!("=== (1) wire ClientMessage::Stop maps to internal ControlMessage::Stop ===");
    let mapped = to_control_message("req-stop-1".to_string(), ClientMessage::Stop);
    println!("  to_control_message(\"req-stop-1\", ClientMessage::Stop) -> {mapped:?}");
    match mapped {
        Some(ControlMessage::Stop {
            request_id,
            context_id,
            force,
        }) => {
            assert_eq!(request_id, "req-stop-1", "request_id must be threaded through unchanged");
            assert_eq!(
                context_id, None,
                "wire Stop carries no context_id -> None -> engages the GLOBAL holo stop kill switch (not a scoped A2A cancel)"
            );
            assert_eq!(force, false, "wire Stop is never force by construction (force is a daemon-internal escalation)");
            println!("  OK -- ClientMessage::Stop -> ControlMessage::Stop{{context_id:None, force:false}}");
        }
        other => panic!("expected Some(ControlMessage::Stop{{..}}), got {other:?}"),
    }

    println!();
    println!("=== (2) holo stop invocation is constructed correctly for both force values ===");
    let graceful = build_stop_args(false);
    let forced = build_stop_args(true);
    println!("  build_stop_args(false) -> {graceful:?}");
    println!("  build_stop_args(true)  -> {forced:?}");
    assert_eq!(graceful, vec!["stop"], "graceful stop must invoke `holo stop`");
    assert_eq!(
        forced,
        vec!["stop", "--force"],
        "force stop must invoke `holo stop --force` (the SIGKILL escalation)"
    );
    println!("  OK -- `holo stop` / `holo stop --force` argument vectors constructed correctly");

    println!();
    println!("=== (3) real `holo stop` fires against the installed holo-desktop-cli ===");
    let holo_bin = match real_holo_bin() {
        Some(path) => {
            let path_str = path.to_string_lossy().to_string();
            println!("  found real holo binary at {path_str}");
            let result = holo_stop(&path_str, false).await;
            println!("  holo_stop(<real holo>, force=false) -> {result:?}");
            assert!(
                result.is_ok(),
                "real `holo stop` (no in-flight turn = benign no-op) must exit Ok; got {result:?}"
            );
            println!("  OK -- real `holo stop` invoked and returned Ok (benign no-op, no turn in flight)");
            path_str
        }
        None => {
            println!("  SKIPPED -- ~/.holo/bin/holo not found in this environment; not faking a real invocation.");
            // Fall back to the bare name so the bridge test below still constructs a Command,
            // even though it would fail to spawn -- covered honestly by step (5)'s error path.
            "holo".to_string()
        }
    };

    println!();
    println!("=== (4) HoloControlBridge::handle(Stop{{context_id:None}}) drains the queue + runs holo stop ===");
    let (bridge, mut rx) = unreachable_bridge(&holo_bin);
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "running".to_string(),
            text: "long task".to_string(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            bridge
                .handle(ControlMessage::Prompt {
                    request_id: "queued-1".to_string(),
                    text: "second task".to_string(),
                    context_id: None,
                })
                .await;
        },
        async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            // This is the kill-switch entry point: exactly what the iOS `Stop` control produces
            // once its `ClientMessage::Stop` reaches `to_control_message` (context_id: None).
            bridge
                .handle(ControlMessage::Stop {
                    request_id: "stop-1".to_string(),
                    context_id: None,
                    force: false,
                })
                .await;
        }
    );
    let events = drain(&mut rx);
    for e in &events {
        println!("  event: {e:?}");
    }
    let stop_ack = events
        .iter()
        .any(|e| matches!(e, ControlEvent::Ack { request_id } if request_id == "stop-1"));
    println!("  Ack for stop-1 present -> {stop_ack}");
    assert!(stop_ack, "handle_stop must Ack the stop request");
    let queued_canceled = events.iter().any(|e| {
        matches!(e, ControlEvent::Done { request_id, status: DoneStatus::Canceled, .. } if request_id == "queued-1")
    });
    println!("  Done{{Canceled}} for queued-1 present -> {queued_canceled}");
    assert!(queued_canceled, "a queued prompt must be dropped with Done{{Canceled}} by the stop");
    let stop_done = events.iter().any(|e| {
        matches!(e, ControlEvent::Done { request_id, status: DoneStatus::Canceled, .. } if request_id == "stop-1")
    });
    println!("  Done{{Canceled}} for stop-1 itself present -> {stop_done}");
    assert!(stop_done, "the stop request itself must resolve with Done{{Canceled}} after holo stop succeeds");
    let (busy_after, queued_after) = bridge.busy_state();
    println!("  busy_state() after stop -> busy={busy_after} queued={queued_after}");
    assert_eq!(queued_after, 0, "queue must be empty after the stop drains it");
    // No ControlEvent::Error naming a holo stop failure -- when the real binary is present, the
    // holo stop sub-call inside handle_stop must have succeeded.
    if real_holo_bin().is_some() {
        let holo_stop_failed = events.iter().any(|e| {
            matches!(e, ControlEvent::Error { message, .. } if message.contains("holo stop failed"))
        });
        println!("  any 'holo stop failed' error -> {holo_stop_failed} (expected false: real holo present)");
        assert!(!holo_stop_failed, "with the real holo binary present, holo stop must not error");
    }
    println!("  OK -- stop Ack'd, queued prompt canceled, stop resolved, queue drained, holo stop ran");

    println!();
    println!("=== (5) bogus holo_bin: Stop surfaces a graceful ControlEvent::Error, never panics ===");
    let (bridge, mut rx) = unreachable_bridge("/nonexistent/definitely-not-holo");
    bridge
        .handle(ControlMessage::Stop {
            request_id: "stop-bogus".to_string(),
            context_id: None,
            force: false,
        })
        .await;
    let events = drain(&mut rx);
    for e in &events {
        println!("  event: {e:?}");
    }
    let stop_failed_error = events.iter().any(|e| {
        matches!(e, ControlEvent::Error { request_id, message } if request_id == "stop-bogus" && message.contains("holo stop failed"))
    });
    println!("  ControlEvent::Error 'holo stop failed' for stop-bogus present -> {stop_failed_error}");
    assert!(
        stop_failed_error,
        "a missing/unspawnable holo binary must surface a graceful 'holo stop failed' error, not panic"
    );
    // And crucially: no Done{Canceled} for the stop itself -- handle_stop returns early on the
    // holo_stop error, so the stop does not falsely report success.
    let false_success = events.iter().any(|e| {
        matches!(e, ControlEvent::Done { request_id, .. } if request_id == "stop-bogus")
    });
    println!("  Done for stop-bogus present -> {false_success} (expected false: holo stop failed)");
    assert!(!false_success, "a failed holo stop must not also emit a success Done for the stop request");
    println!("  OK -- kill switch degrades cleanly when the holo CLI is missing");

    println!();
    println!(
        "holo_stop_probe: OK -- remote kill-switch daemon path witnessed via real execution: \
         ClientMessage::Stop -> ControlMessage::Stop{{context_id:None}} mapping, correct \
         `holo stop`/`holo stop --force` command construction, a real `holo stop` invocation \
         against the installed holo-desktop-cli, the full handle_stop queue-drain + holo-stop \
         path, and graceful ControlEvent::Error on a missing holo binary."
    );
}
