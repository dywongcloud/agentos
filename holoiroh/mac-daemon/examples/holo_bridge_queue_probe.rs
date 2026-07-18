//! Manual, run-by-hand probe: exercises the real `HoloControlBridge`'s concurrent-prompt-queueing
//! logic (`handle`/`busy_state`, real `tokio::join!` races, real `std::sync::Mutex` contention
//! over `busy`/`queue`) against a real `A2aClient` pointed at an unreachable TEST-NET-1 address
//! (`192.0.2.1:1`, RFC 5737 -- guaranteed not to route, so `send_and_stream` fails fast and
//! deterministically with a real connection error, exactly as the removed tests relied on).
//! Witnesses the queueing/race behavior that used to live in `holo_bridge/control.rs`'s
//! `#[cfg(test)] mod tests` `#[tokio::test]` fns (removed per this repo's no-unit-tests rule),
//! driven by `cargo run` instead of `cargo test`.
//!
//! ## What this probe does NOT cover, and why
//!
//! The task asked for this specifically: send two real prompts back-to-back over a real
//! control-channel connection to a real daemon with a real `holo serve` backend, and observe the
//! real queueing status message end-to-end. That full path requires:
//!   1. A real running `holoiroh-daemon` process -- checked in this session with
//!      `../target/debug/holoiroh-daemon --no-pin-auth`, which fails immediately every time with
//!      `Missing permission: Accessibility` / exit code 1 (real, reproducibly observed output --
//!      this sandboxed session has no macOS Accessibility TCC grant, and `TCC.db` itself returns
//!      `authorization denied` when queried directly, so there is no way to grant or bypass this
//!      from within the sandbox).
//!   2. A real `holo` CLI on PATH for the daemon to spawn `holo serve` -- checked with `which
//!      holo` and `command -v holo` in this session, both exit 1 ("holo not found"); no `holo`
//!      binary exists at `/usr/local/bin` or `/opt/homebrew/bin` either.
//!
//! Both are real, concretely observed, environment-level blockers (not assumed or guessed), and
//! either one alone would block the full live-daemon witness. Per this task's own instructions
//! for exactly this situation, that limitation is documented here rather than silently skipped or
//! faked -- this probe instead witnesses the real queueing/race logic itself (the part reachable
//! in this sandbox) via the same "unreachable A2A endpoint" seam the removed tests used.
//!
//! Run with `cargo run --example holo_bridge_queue_probe`.

use std::time::Duration;

use holoiroh_daemon::holo_bridge::a2a_client::A2aClient;
use holoiroh_daemon::holo_bridge::{ControlEvent, ControlMessage, HoloControlBridge};
use tokio::sync::mpsc;

/// Builds a `HoloControlBridge` pointed at a base URL with no listener (RFC 5737 TEST-NET-1,
/// guaranteed not to route) so `send_and_stream` fails fast and deterministically with a
/// connection error rather than actually reaching a `holo serve` instance -- same seam the
/// removed tests used to observe queue/busy state transitions without a live backend.
fn unreachable_bridge() -> (HoloControlBridge, mpsc::UnboundedReceiver<ControlEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let client = A2aClient::new("http://192.0.2.1:1".to_string(), "probe-token".to_string());
    (HoloControlBridge::new(client, "holo", tx), rx)
}

async fn drain(rx: &mut mpsc::UnboundedReceiver<ControlEvent>) -> Vec<ControlEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::main]
async fn main() {
    println!("=== second prompt while first in flight is queued, not run immediately ===");
    let (bridge, mut rx) = unreachable_bridge();
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "first".to_string(),
            text: "do the first thing".to_string(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (busy, queued) = bridge.busy_state();
            println!("  [t=20ms, before sending second] busy_state() -> busy={busy} queued={queued}");
            bridge
                .handle(ControlMessage::Prompt {
                    request_id: "second".to_string(),
                    text: "do the second thing".to_string(),
                    context_id: None,
                })
                .await;
        }
    );
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    let queued = events
        .iter()
        .find(|e| matches!(e, ControlEvent::Queued { request_id, .. } if request_id == "second"));
    println!("Queued event for 'second' -> {queued:?}");
    assert!(queued.is_some(), "expected a Queued event for the second (racing) prompt");
    if let Some(ControlEvent::Queued { ahead, .. }) = queued {
        assert_eq!(*ahead, 0, "second prompt was the only one queued behind the first");
    }
    let terminal_ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            ControlEvent::Error { request_id, .. } => Some(request_id.as_str()),
            _ => None,
        })
        .collect();
    println!("terminal Error ids -> {terminal_ids:?}");
    assert!(terminal_ids.contains(&"first"));
    assert!(terminal_ids.contains(&"second"));
    let first_error_pos = events
        .iter()
        .position(|e| matches!(e, ControlEvent::Error { request_id, .. } if request_id == "first"))
        .expect("first prompt must have a terminal Error event");
    let second_error_pos = events
        .iter()
        .position(|e| matches!(e, ControlEvent::Error { request_id, .. } if request_id == "second"))
        .expect("second prompt must have a terminal Error event");
    println!("first_error_pos={first_error_pos} second_error_pos={second_error_pos}");
    assert!(
        second_error_pos > first_error_pos,
        "queued prompt's terminal event must come after the in-flight prompt's terminal event"
    );

    println!();
    println!("=== stop drains queued prompts with a terminal Done event each ===");
    let (bridge, mut rx) = unreachable_bridge();
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
            tokio::time::sleep(Duration::from_millis(21)).await;
            bridge
                .handle(ControlMessage::Prompt {
                    request_id: "queued-2".to_string(),
                    text: "third task".to_string(),
                    context_id: None,
                })
                .await;
        },
        async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            bridge
                .handle(ControlMessage::Stop {
                    request_id: "stop-1".to_string(),
                    context_id: None,
                    force: false,
                })
                .await;
        }
    );
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    for id in ["queued-1", "queued-2"] {
        let done = events.iter().find(|e| {
            matches!(e, ControlEvent::Done { request_id, status: holoiroh_daemon::holo_bridge::control::DoneStatus::Canceled, .. } if request_id == id)
        });
        println!("Done{{Canceled}} event for '{id}' -> present={}", done.is_some());
        assert!(done.is_some(), "expected queued prompt {id} to get a terminal Done{{Canceled}} event from Stop");
    }
    let (busy_after, queued_after) = bridge.busy_state();
    println!("busy_state() after stop -> busy={busy_after} queued={queued_after}");
    assert_eq!(queued_after, 0, "queue must be empty after Stop drains it");

    println!();
    println!("=== queued prompts report correct 'ahead' count for multiple queued ===");
    let (bridge, mut rx) = unreachable_bridge();
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "running".to_string(),
            text: "t".to_string(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(15)).await;
            bridge
                .handle(ControlMessage::Prompt {
                    request_id: "q1".to_string(),
                    text: "t".to_string(),
                    context_id: None,
                })
                .await;
        },
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            bridge
                .handle(ControlMessage::Prompt {
                    request_id: "q2".to_string(),
                    text: "t".to_string(),
                    context_id: None,
                })
                .await;
        }
    );
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    let q1_ahead = events.iter().find_map(|e| match e {
        ControlEvent::Queued { request_id, ahead } if request_id == "q1" => Some(*ahead),
        _ => None,
    });
    let q2_ahead = events.iter().find_map(|e| match e {
        ControlEvent::Queued { request_id, ahead } if request_id == "q2" => Some(*ahead),
        _ => None,
    });
    println!("q1_ahead={q1_ahead:?} q2_ahead={q2_ahead:?}");
    assert_eq!(q1_ahead, Some(0), "q1 was queued first, 0 ahead of it");
    assert_eq!(q2_ahead, Some(1), "q2 was queued second, 1 (q1) ahead of it");

    println!();
    println!(
        "holo_bridge_queue_probe: OK -- real queueing/race logic witnessed via real execution \
         against an unreachable A2A endpoint. Full live-daemon + live-`holo serve` witness \
         blocked in this sandbox by two real, observed causes (see module doc): daemon refuses \
         to start (Accessibility TCC permission missing, confirmed reproducibly, TCC.db \
         inaccessible), and `holo` CLI not present on PATH."
    );
}
