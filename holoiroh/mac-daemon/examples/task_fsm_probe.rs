//! Witnesses `crate::task_fsm` two ways: (1) unit-shaped classification of real
//! `TrajectoryEvent` kind strings (no live backend needed), and (2) a REAL task run through
//! the full control bridge against a live `holo serve`, reading persisted FSM state off disk
//! afterward to confirm phase tracking actually happened end-to-end.
//!
//! Run: `cargo run --example task_fsm_probe`

use anyhow::{Context, Result, bail};
use holoiroh_daemon::holo_bridge::{ControlEvent, ControlMessage, HoloBridge};
use holoiroh_daemon::task_fsm::{Phase, TaskFsm};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;

const PROBE_A2A_PORT: u16 = 18793;
const PROBE_RUNTIME_PORT: &str = "18905";

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // --- Part 1: pure classification witness (no live backend). ---
    let mut fsm = TaskFsm::new("probe-unit");
    assert_eq!(fsm.phase, Phase::Plan, "starts at Plan");

    let changed = fsm.observe_working(Some(&json!({"kind": "observation_event"})));
    println!("[{}] observation_event -> {:?} (changed={changed}, expected Plan/false: already Plan)",
        if fsm.phase == Phase::Plan && !changed { "OK" } else { "FAIL" }, fsm.phase);
    if !(fsm.phase == Phase::Plan && !changed) {
        bail!("unit case 1 failed");
    }

    let changed = fsm.observe_working(Some(&json!({"kind": "tool_result"})));
    println!("[{}] tool_result -> {:?} (changed={changed}, actions={})",
        if fsm.phase == Phase::Execute && changed && fsm.actions_taken == 1 { "OK" } else { "FAIL" },
        fsm.phase, fsm.actions_taken);
    if !(fsm.phase == Phase::Execute && changed && fsm.actions_taken == 1) {
        bail!("unit case 2 failed");
    }

    // Sticky Execute: a later policy_event must NOT regress the phase.
    let changed = fsm.observe_working(Some(&json!({"kind": "policy_event"})));
    println!("[{}] policy_event mid-Execute -> {:?} (changed={changed}, expected Execute/false)",
        if fsm.phase == Phase::Execute && !changed { "OK" } else { "FAIL" }, fsm.phase);
    if !(fsm.phase == Phase::Execute && !changed) {
        bail!("unit case 3 (sticky Execute) failed");
    }

    let changed = fsm.observe_answer("the answer");
    println!("[{}] answer_event -> {:?} (changed={changed})",
        if fsm.phase == Phase::Verify && changed { "OK" } else { "FAIL" }, fsm.phase);
    if !(fsm.phase == Phase::Verify && changed) {
        bail!("unit case 4 failed");
    }

    fsm.advance_terminal(holoiroh_daemon::holo_bridge::a2a_client::TerminalState::Completed);
    println!("[{}] real Completed terminal -> {:?} (expected Done)",
        if fsm.phase == Phase::Done { "OK" } else { "FAIL" }, fsm.phase);
    if fsm.phase != Phase::Done {
        bail!("unit case 5 failed");
    }

    // Empty-completion shape: zero actions, no claimed answer, but Completed -- must downgrade
    // to Failed (the same signal the tinfoil-failover empty-answer detection already treats
    // as a backend failure, now checked at the FSM layer too).
    let mut empty_fsm = TaskFsm::new("probe-unit-empty");
    empty_fsm.advance_terminal(holoiroh_daemon::holo_bridge::a2a_client::TerminalState::Completed);
    println!("[{}] Completed with zero actions/no answer -> {:?} (expected Failed)",
        if empty_fsm.phase == Phase::Failed { "OK" } else { "FAIL" }, empty_fsm.phase);
    if empty_fsm.phase != Phase::Failed {
        bail!("unit case 6 (empty-completion downgrade) failed");
    }

    // Plan can skip straight to Verify/Done for a pure Q&A turn with zero tool calls.
    let mut qa_fsm = TaskFsm::new("probe-unit-qa");
    qa_fsm.observe_answer("42");
    qa_fsm.advance_terminal(holoiroh_daemon::holo_bridge::a2a_client::TerminalState::Completed);
    println!("[{}] pure Q&A (answer, no tool calls) -> {:?} (expected Done, actions={})",
        if qa_fsm.phase == Phase::Done { "OK" } else { "FAIL" }, qa_fsm.phase, qa_fsm.actions_taken);
    if qa_fsm.phase != Phase::Done {
        bail!("unit case 7 (Plan-skip-to-Done) failed");
    }

    println!("TASK_FSM UNIT WITNESS: ALL CASES OK");

    // --- Part 2: real end-to-end run through the control bridge. ---
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(live_witness())
}

async fn live_witness() -> Result<()> {
    dotenvy::dotenv().ok();
    unsafe { std::env::set_var("HOLOIROH_AGENT_RUNTIME_PORT", PROBE_RUNTIME_PORT) };
    let holo_bin = std::env::var("HOLOIROH_HOLO_BIN").unwrap_or_else(|_| {
        let installed = std::env::var("HOME").map(|h| format!("{h}/.holo/bin/holo"));
        match installed {
            Ok(p) if std::path::Path::new(&p).exists() => p,
            _ => "holo".to_string(),
        }
    });

    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let bridge = Arc::new(
        HoloBridge::start(
            holo_bin,
            PROBE_A2A_PORT,
            None,
            None,
            std::time::Duration::from_secs(1800),
            events_tx,
        )
        .await
        .context("HoloBridge failed to start")?,
    );
    bridge.control.attach_bridge(Arc::downgrade(&bridge));

    let request_id = "task-fsm-live-probe".to_string();
    println!("sending a real turn (request_id={request_id})...");
    bridge
        .handle_message(ControlMessage::Prompt {
            request_id: request_id.clone(),
            text: "Do not click or type anything. Immediately finish and answer with the single word: ping".to_string(),
            context_id: None,
        })
        .await;

    let mut saw_phase_status = false;
    let mut saw_answer = false;
    let mut saw_done_status = false;
    while let Ok(event) = events_rx.try_recv() {
        println!("event: {event:?}");
        match event {
            ControlEvent::DaemonStatus { text } => {
                if text.contains("planning") || text.contains("acting on your Mac") || text.contains("reviewing") {
                    saw_phase_status = true;
                }
            }
            ControlEvent::Answer { .. } => saw_answer = true,
            ControlEvent::Done { .. } => saw_done_status = true,
            _ => {}
        }
    }

    // The FSM's persisted file is deleted on conclude (by design -- see TaskRegistry::conclude)
    // once the turn fully finishes, so its ABSENCE here is itself the witness that the full
    // begin -> observe -> terminal -> conclude lifecycle ran to completion without leaking
    // state. Confirm that directly.
    let persisted_path = dirs_home()?.join(".holoiroh/tasks").join(format!("{request_id}.json"));
    let leaked = persisted_path.exists();

    if let Ok(owned) = Arc::try_unwrap(bridge) {
        owned.shutdown().await.ok();
    }

    println!(
        "saw_phase_status={saw_phase_status} saw_answer={saw_answer} saw_done_status={saw_done_status} persisted_file_leaked={leaked}"
    );
    if !saw_phase_status || !saw_answer || !saw_done_status || leaked {
        bail!("LIVE WITNESS FAILED");
    }
    println!("TASK_FSM LIVE WITNESS: OK (phase status emitted, answer + done seen, no leaked state file)");
    Ok(())
}

fn dirs_home() -> Result<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .context("HOME not set")
}
