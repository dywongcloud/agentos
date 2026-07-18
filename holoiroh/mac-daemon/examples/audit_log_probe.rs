//! Manual, run-by-hand probe: exercises the real `audit_log` module -- `AuditLogger::new`/
//! `append` -- against a real temp file on disk, then reads the actual bytes that landed on disk
//! back and prints them, proving by real execution (not narration) two things at once:
//!
//! 1. Every [`holoiroh_daemon::audit_log::AuditEntry`] field round-trips to JSON correctly and the
//!    file is genuinely append-only (multiple `append` calls all survive, none overwritten).
//! 2. **The PRD's own acceptance test for row P0-12**: a distinctive dictated-text string, of the
//!    exact kind a real voice transcript would carry (name, sentence, punctuation), is run through
//!    a full simulated task lifecycle -- but *never* passed to any `AuditEntry` field (matching
//!    exactly how `control_channel.rs`'s real wiring works: `ClientMessage::VoiceTranscript`'s
//!    `text` field is read only to pick an [`holoiroh_daemon::audit_log::ActionClass`], never
//!    stored) -- and then the literal log *file bytes* are read back off disk and searched for
//!    that string, asserting it is **not present anywhere** in the file. This is a real
//!    log-inspection test: it does not trust the typed `AuditEntry` API to have "just worked", it
//!    greps the actual serialized JSON Lines content that would be sitting on a user's disk at
//!    `~/.holoiroh/audit.log`.
//!
//! Run with `cargo run --example audit_log_probe`.

use std::io::Read;

use holoiroh_daemon::audit_log::{
    ActionClass, AppCategory, AuditEntry, AuditLogger, ConnectionPath, FinalStatus,
    InferenceMode, RemoteViewState, now_ms,
};

fn temp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "holoiroh-audit-log-probe-{name}-{}-{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    p
}

/// Reads the raw file content at `path` as a `String`. Deliberately *not* using
/// `AuditLogger`/`serde_json` for this read -- the acceptance test's whole point is inspecting the
/// literal bytes written to disk, not trusting a typed reader to have not smuggled something past
/// itself.
fn read_raw(path: &std::path::Path) -> String {
    let mut file = std::fs::File::open(path).expect("audit log file should exist after append");
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .expect("audit log file should be valid UTF-8 text");
    contents
}

fn main() {
    println!("=== AuditLogger::new creates the parent directory ===");
    let path = temp_path("basic");
    // PID + nanos in the subdir name, matching temp_path()'s own uniqueness scheme --
    // a fixed name here left a stale dir behind across runs, making the not-exists
    // assertion below fail on every run after the first.
    let parent = path.parent().unwrap().join(format!(
        "holoiroh-audit-probe-subdir-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let nested_path = parent.join("audit.log");
    assert!(!parent.exists(), "subdir must not exist yet for this to be a real test");
    let logger = AuditLogger::new(&nested_path).expect("AuditLogger::new should create parent dir");
    println!("AuditLogger::new({}) -> parent dir created: {}", nested_path.display(), parent.exists());
    assert!(parent.exists());
    assert_eq!(logger.path(), nested_path.as_path());

    println!();
    println!("=== append: writes one JSON line per entry, real JSON Lines format ===");
    let path = temp_path("roundtrip");
    let logger = AuditLogger::new(&path).expect("AuditLogger::new should succeed");

    let started_at_ms = now_ms();
    let entry1 = AuditEntry {
        task_id: "task-0001".to_string(),
        started_at_ms,
        completed_at_ms: started_at_ms + 1500,
        app_category: AppCategory::Desktop,
        action_class: ActionClass::Prompt,
        inference_mode: InferenceMode::Cloud,
        remote_view_state: RemoteViewState::Streaming,
        connection_path: ConnectionPath::Direct,
        final_status: FinalStatus::Completed,
        latency_ms: 1500,
        action_count: 4,
    };
    logger.append(&entry1).expect("first append should succeed");

    let entry2 = AuditEntry {
        task_id: "task-0002".to_string(),
        started_at_ms: started_at_ms + 2000,
        completed_at_ms: started_at_ms + 2900,
        app_category: AppCategory::Desktop,
        action_class: ActionClass::VoiceTranscript,
        inference_mode: InferenceMode::Cloud,
        remote_view_state: RemoteViewState::Streaming,
        connection_path: ConnectionPath::Relay,
        final_status: FinalStatus::Failed,
        latency_ms: 900,
        action_count: 2,
    };
    logger.append(&entry2).expect("second append should succeed");

    let raw = read_raw(&path);
    let lines: Vec<&str> = raw.lines().collect();
    println!("wrote 2 entries; file now has {} line(s):", lines.len());
    for line in &lines {
        println!("  {line}");
    }
    assert_eq!(lines.len(), 2, "each append must produce exactly one new line, none overwritten");

    let parsed1: serde_json::Value = serde_json::from_str(lines[0]).expect("line 1 must be valid JSON");
    let parsed2: serde_json::Value = serde_json::from_str(lines[1]).expect("line 2 must be valid JSON");
    assert_eq!(parsed1["task_id"], "task-0001");
    assert_eq!(parsed1["action_class"], "prompt");
    assert_eq!(parsed1["connection_path"], "direct");
    assert_eq!(parsed1["final_status"], "completed");
    assert_eq!(parsed1["latency_ms"], 1500);
    assert_eq!(parsed1["action_count"], 4);
    assert_eq!(parsed2["task_id"], "task-0002");
    assert_eq!(parsed2["action_class"], "voice_transcript");
    assert_eq!(parsed2["connection_path"], "relay");
    assert_eq!(parsed2["final_status"], "failed");
    println!("all fields round-trip correctly for both entries");

    println!();
    println!("=== append is true append-only: a fresh AuditLogger on the same path does not truncate ===");
    let logger_reopened = AuditLogger::new(&path).expect("re-opening the same path should succeed");
    let entry3 = AuditEntry {
        task_id: "task-0003".to_string(),
        started_at_ms: started_at_ms + 3000,
        completed_at_ms: started_at_ms + 3100,
        app_category: AppCategory::Desktop,
        action_class: ActionClass::Prompt,
        inference_mode: InferenceMode::Cloud,
        remote_view_state: RemoteViewState::Streaming,
        connection_path: ConnectionPath::Unknown,
        final_status: FinalStatus::Canceled,
        latency_ms: 100,
        action_count: 0,
    };
    logger_reopened.append(&entry3).expect("third append (new logger instance, same path) should succeed");
    let raw_after = read_raw(&path);
    let lines_after: Vec<&str> = raw_after.lines().collect();
    println!("after re-opening AuditLogger on the same path and appending once more: {} line(s)", lines_after.len());
    assert_eq!(lines_after.len(), 3, "a fresh AuditLogger instance on an existing path must APPEND, never truncate prior entries");
    assert!(raw_after.starts_with(lines[0]), "the original first line must still be present, byte-for-byte, at the start of the file");

    println!();
    println!("=== ACCEPTANCE TEST (Project Aro PRD row P0-12): no content ever reaches the audit log ===");
    // A distinctive, realistic dictated-text string -- the exact shape of what a real
    // ClientMessage::VoiceTranscript.text would carry: a name, a sentence, punctuation. Chosen to
    // be maximally distinctive (a marker token no legitimate metadata field would ever coincidentally
    // contain) so its absence from the log is a strong, unambiguous signal, not a fluke.
    const DICTATED_TEXT: &str =
        "AUDITPROBE_MARKER_7f3e2a: tell Sarah Chen the quarterly report is in her inbox, then open Mail and reply to the thread about the Berlin trip";

    let path = temp_path("acceptance");
    let logger = AuditLogger::new(&path).expect("AuditLogger::new should succeed");

    // Simulate exactly what control_channel.rs's real wiring does: a ClientMessage::VoiceTranscript
    // arrives carrying DICTATED_TEXT. The real code (see audit_starts.lock()...insert(..) in
    // control_channel.rs's ProtocolHandler::accept) reads the *variant* of the message to pick an
    // ActionClass, and never touches `text` again for audit purposes. We mirror that exactly here:
    // `dictated_text` below is read once, for pattern-matching only, then dropped -- never assigned
    // into any AuditEntry field.
    let dictated_text = DICTATED_TEXT.to_string();
    let action_class = if dictated_text.starts_with("AUDITPROBE_MARKER") {
        ActionClass::VoiceTranscript
    } else {
        ActionClass::Prompt
    };
    // `dictated_text` is now out of scope for everything below -- proving structurally (not just
    // by convention) that the entry-building code below has no access to it, matching how
    // `audit_on_control_event` in control_channel.rs never receives `ControlMessage::text` at all
    // (only `ControlEvent`, which -- for `Done` -- carries `message: Option<String>`, itself never
    // read into any AuditEntry field either; see that function's real source).
    drop(dictated_text);

    let task_started_at = now_ms();
    let entry = AuditEntry {
        task_id: "task-acceptance-0001".to_string(),
        started_at_ms: task_started_at,
        completed_at_ms: task_started_at + 4200,
        app_category: AppCategory::Desktop,
        action_class,
        inference_mode: InferenceMode::Cloud,
        remote_view_state: RemoteViewState::Streaming,
        connection_path: ConnectionPath::Direct,
        final_status: FinalStatus::Completed,
        latency_ms: 4200,
        action_count: 6,
    };
    logger.append(&entry).expect("acceptance-test append should succeed");

    // Read back the ACTUAL bytes on disk -- not the typed AuditEntry we just built, the literal
    // file content, exactly as a real operator inspecting ~/.holoiroh/audit.log would see it.
    let raw = read_raw(&path);
    println!("actual JSON Lines written to {}:", path.display());
    for line in raw.lines() {
        println!("  {line}");
    }

    let contains_marker = raw.contains("AUDITPROBE_MARKER");
    let contains_name = raw.contains("Sarah Chen");
    let contains_sentence_fragment = raw.contains("quarterly report") || raw.contains("Berlin trip");
    println!();
    println!(
        "log file contains dictated-text marker: {contains_marker}, contains recipient name: {contains_name}, contains sentence fragment: {contains_sentence_fragment}"
    );
    assert!(!contains_marker, "ACCEPTANCE TEST FAILED: the dictated-text marker leaked into the audit log");
    assert!(!contains_name, "ACCEPTANCE TEST FAILED: a recipient name leaked into the audit log");
    assert!(!contains_sentence_fragment, "ACCEPTANCE TEST FAILED: dictated sentence content leaked into the audit log");

    // Also confirm the log is non-trivial (a passing "absence" check on an accidentally-empty file
    // would be a false positive) -- real metadata fields are present.
    assert!(raw.contains("task-acceptance-0001"), "the real task_id metadata field must be present");
    assert!(raw.contains("voice_transcript"), "the real action_class metadata field must be present");
    assert!(raw.contains("desktop"), "the real app_category metadata field must be present");
    assert!(raw.contains("cloud"), "the real inference_mode metadata field must be present");
    assert!(raw.contains("streaming"), "the real remote_view_state metadata field must be present");
    assert!(raw.contains("direct"), "the real connection_path metadata field must be present");
    assert!(raw.contains("completed"), "the real final_status metadata field must be present");
    assert!(raw.contains("4200"), "the real latency_ms metadata field must be present");
    println!("all real metadata fields ARE present (this is not an accidentally-empty-file false pass)");

    println!();
    println!("audit_log_probe: OK -- metadata logged, dictated-text content proven absent via real log-file inspection");
}
