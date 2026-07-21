//! Manual, run-by-hand probe: witnesses the pause/resume/redirect control surface added to
//! `HoloControlBridge`, plus its degenerate states, via real execution against the same
//! unreachable TEST-NET-1 A2A endpoint seam `holo_bridge_queue_probe` uses (turns start for
//! real and fail fast with a real connection error -- enough lifetime to interrupt them).
//!
//! Covered here (each printed + asserted):
//! 1. Wire mapping: `ClientMessage::{Pause,Resume,Redirect}` -> `ControlMessage` shapes.
//! 2. Pause with nothing running -> polite status, no crash.
//! 3. Resume with nothing paused -> polite status, no crash.
//! 4. Redirect with empty text -> `Error`, nothing dispatched.
//! 5. Pause mid-flight stashes the turn; a following Resume re-dispatches it (fresh turn
//!    observed as its own terminal event under the RESUME's request_id).
//! 6. Stop discards a paused stash ("stop: discarded the paused task" status), and a Resume
//!    afterwards finds nothing.
//! 7. Redirect mid-flight: queued prompts drain with Done{Canceled}, and the redirect's own
//!    turn runs after the in-flight one dies.
//!
//! NOT covered: the sensitive-app consent round trip (`resolve_consent`) -- it requires a
//! real `Arc<HoloBridge>` (a live `holo serve`), which this endpoint-less probe deliberately
//! avoids; that path is witnessed live against the running daemon instead.
//!
//! Run with `cargo run --example pause_redirect_probe`.

use std::time::Duration;

use holoiroh_daemon::control_channel::to_control_message;
use holoiroh_daemon::holo_bridge::a2a_client::A2aClient;
use holoiroh_daemon::holo_bridge::control::DoneStatus;
use holoiroh_daemon::holo_bridge::{ControlEvent, ControlMessage, HoloControlBridge};
use holoiroh_wire::ClientMessage;
use tokio::sync::mpsc;

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

fn statuses(events: &[ControlEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ControlEvent::DaemonStatus { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::main]
async fn main() {
    println!("=== 1. wire ClientMessage -> internal ControlMessage mapping ===");
    let pause = to_control_message("r1".into(), ClientMessage::Pause);
    println!("  Pause -> {pause:?}");
    assert!(matches!(pause, Some(ControlMessage::Pause { ref request_id }) if request_id == "r1"));
    let resume = to_control_message("r2".into(), ClientMessage::Resume);
    println!("  Resume -> {resume:?}");
    assert!(matches!(resume, Some(ControlMessage::Resume { ref request_id }) if request_id == "r2"));
    let redirect = to_control_message(
        "r3".into(),
        ClientMessage::Redirect {
            text: "new plan".into(),
        },
    );
    println!("  Redirect -> {redirect:?}");
    assert!(matches!(
        redirect,
        Some(ControlMessage::Redirect { ref request_id, ref text }) if request_id == "r3" && text == "new plan"
    ));

    println!();
    println!("=== 2. pause with nothing running is a polite status ===");
    let (bridge, mut rx) = unreachable_bridge();
    bridge
        .handle(ControlMessage::Pause {
            request_id: "p-idle".into(),
        })
        .await;
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    let s = statuses(&events);
    assert!(
        s.iter().any(|t| t.contains("no task to pause")),
        "expected the nothing-running status, got {s:?}"
    );

    println!();
    println!("=== 3. resume with nothing paused is a polite status ===");
    bridge
        .handle(ControlMessage::Resume {
            request_id: "r-idle".into(),
        })
        .await;
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    let s = statuses(&events);
    assert!(
        s.iter().any(|t| t.contains("no task to resume")),
        "expected the nothing-paused status, got {s:?}"
    );

    println!();
    println!("=== 4. redirect with empty text is an Error, nothing dispatched ===");
    bridge
        .handle(ControlMessage::Redirect {
            request_id: "rd-empty".into(),
            text: "   ".into(),
        })
        .await;
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            ControlEvent::Error { request_id, message } if request_id == "rd-empty" && message.contains("non-empty")
        )),
        "expected the empty-redirect Error"
    );
    let (busy, queued) = bridge.busy_state();
    assert!(!busy && queued == 0, "empty redirect must not start/queue anything");

    println!();
    println!("=== 5. pause mid-flight stashes; resume re-dispatches under the resume id ===");
    let (bridge, mut rx) = unreachable_bridge();
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "task-a".into(),
            text: "long task".into(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            bridge
                .handle(ControlMessage::Pause {
                    request_id: "pause-a".into(),
                })
                .await;
        }
    );
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    let s = statuses(&events);
    assert!(
        s.iter().any(|t| t.contains("task paused")),
        "expected the task-paused status, got {s:?}"
    );
    bridge
        .handle(ControlMessage::Resume {
            request_id: "resume-a".into(),
        })
        .await;
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    assert!(
        statuses(&events).iter().any(|t| t.contains("resuming")),
        "expected the resuming status"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ControlEvent::Error { request_id, .. } if request_id == "resume-a"
        )),
        "resumed turn must run (and, on this unreachable endpoint, fail) under the RESUME's request_id"
    );
    // Second resume finds nothing -- the stash was consumed.
    bridge
        .handle(ControlMessage::Resume {
            request_id: "resume-a2".into(),
        })
        .await;
    let events = drain(&mut rx).await;
    assert!(
        statuses(&events).iter().any(|t| t.contains("no task to resume")),
        "double-resume must find an empty stash"
    );

    println!();
    println!("=== 6. stop discards a paused stash ===");
    let (bridge, mut rx) = unreachable_bridge();
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "task-b".into(),
            text: "another task".into(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            bridge
                .handle(ControlMessage::Pause {
                    request_id: "pause-b".into(),
                })
                .await;
        }
    );
    drain(&mut rx).await;
    bridge
        .handle(ControlMessage::Stop {
            request_id: "stop-b".into(),
            context_id: None,
            force: false,
        })
        .await;
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    assert!(
        statuses(&events).iter().any(|t| t.contains("discarded the paused task")),
        "stop must announce it discarded the paused stash"
    );
    // The stop's own terminal depends on the environment: with a real `holo`
    // binary on PATH the global kill switch succeeds and a Done{Canceled}
    // follows; on a headless runner with no `holo`, the stop honestly reports
    // an Error instead. Either is a correct account of what happened -- the
    // stash-discard assertions above and the empty-stash resume below are
    // this section's actual claims.
    let stop_concluded = events.iter().any(|e| {
        matches!(e, ControlEvent::Done { request_id, status: DoneStatus::Canceled, .. } if request_id == "stop-b")
            || matches!(e, ControlEvent::Error { request_id, message } if request_id == "stop-b" && message.contains("holo stop"))
    });
    assert!(stop_concluded, "stop must conclude with Done{{Canceled}} or an honest holo-stop Error");
    bridge
        .handle(ControlMessage::Resume {
            request_id: "resume-b".into(),
        })
        .await;
    let events = drain(&mut rx).await;
    assert!(
        statuses(&events).iter().any(|t| t.contains("no task to resume")),
        "resume after stop must find nothing"
    );

    println!();
    println!("=== 7. redirect mid-flight drains the queue and runs the new instruction ===");
    let (bridge, mut rx) = unreachable_bridge();
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "task-c".into(),
            text: "original task".into(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(8)).await;
            bridge
                .handle(ControlMessage::Prompt {
                    request_id: "task-c-queued".into(),
                    text: "queued task".into(),
                    context_id: None,
                })
                .await;
        },
        async {
            tokio::time::sleep(Duration::from_millis(16)).await;
            bridge
                .handle(ControlMessage::Redirect {
                    request_id: "redirect-c".into(),
                    text: "do this instead".into(),
                })
                .await;
        }
    );
    let events = drain(&mut rx).await;
    for e in &events {
        println!("  event: {e:?}");
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            ControlEvent::Done { request_id, status: DoneStatus::Canceled, .. } if request_id == "task-c-queued"
        )),
        "queued prompt must be drained with Done{{Canceled}} by the redirect"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ControlEvent::Error { request_id, .. } if request_id == "redirect-c"
        )),
        "the redirect's own turn must run (and, here, fail on the unreachable endpoint) under its id"
    );
    let (busy, queued) = bridge.busy_state();
    assert!(!busy && queued == 0, "everything settled after the redirect");

    println!();
    println!(
        "pause_redirect_probe: OK -- pause/resume/redirect mappings, mid-flight interruption, \
         and every degenerate state witnessed via real execution."
    );
}
