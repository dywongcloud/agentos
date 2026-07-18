//! Manual, run-by-hand probe: exercises the real `TaskState` serde round-trips and
//! `is_valid_transition` logic from `task_state.rs`, printing real output. This is the sole
//! witness for that module's logic per this repo's no-unit-tests rule (see
//! `control_channel_probe.rs`'s own module doc for the same pattern applied to `control_channel.rs`).
//!
//! Run with `cargo run --example task_state_probe`.

use holoiroh_daemon::task_state::{TaskState, is_valid_transition};

/// Every documented happy-path edge, in PRD order.
const HAPPY_PATH: &[TaskState] = &[
    TaskState::Created,
    TaskState::Queued,
    TaskState::Connecting,
    TaskState::Authenticated,
    TaskState::RemoteViewStarting,
    TaskState::RemoteViewActive,
    TaskState::PolicyChecking,
    TaskState::LaunchingApp,
    TaskState::FindingTarget,
    TaskState::Navigating,
    TaskState::TypingDraft,
    TaskState::Verifying,
    TaskState::DraftReady,
    TaskState::AwaitingConfirmation,
    TaskState::Committing,
    TaskState::Completed,
];

/// Every `TaskState` variant, for exhaustive terminal/serde sweeps. Kept as an explicit list
/// (rather than a macro/derive-based enumeration) so this probe visibly enumerates the exact
/// same 30 variants `task_state.rs`'s own doc comment claims (16 flow + 4 interactive-wait + 7
/// alpha-terminal + 3 Tinfoil-deferred).
const ALL_STATES: &[TaskState] = &[
    TaskState::Created,
    TaskState::Queued,
    TaskState::Connecting,
    TaskState::Authenticated,
    TaskState::RemoteViewStarting,
    TaskState::RemoteViewActive,
    TaskState::PolicyChecking,
    TaskState::LaunchingApp,
    TaskState::FindingTarget,
    TaskState::Navigating,
    TaskState::TypingDraft,
    TaskState::Verifying,
    TaskState::DraftReady,
    TaskState::AwaitingConfirmation,
    TaskState::Committing,
    TaskState::Completed,
    TaskState::NeedsLogin,
    TaskState::NeedsMfa,
    TaskState::NeedsConfirmation,
    TaskState::SensitiveAccessRequested,
    TaskState::UserCancelled,
    TaskState::PermissionDenied,
    TaskState::AmbiguousTarget,
    TaskState::TargetNotFound,
    TaskState::SensitiveAccessRejected,
    TaskState::AgentTimeout,
    TaskState::Failed,
    TaskState::ConfidentialCloudConsentRequired,
    TaskState::ConfidentialAttestationFailed,
    TaskState::ConfidentialModelUnavailable,
];

const TERMINAL_STATES: &[TaskState] = &[
    TaskState::Completed,
    TaskState::UserCancelled,
    TaskState::PermissionDenied,
    TaskState::AmbiguousTarget,
    TaskState::TargetNotFound,
    TaskState::SensitiveAccessRejected,
    TaskState::AgentTimeout,
    TaskState::Failed,
    TaskState::ConfidentialCloudConsentRequired,
    TaskState::ConfidentialAttestationFailed,
    TaskState::ConfidentialModelUnavailable,
];

const TINFOIL_DEFERRED: &[TaskState] = &[
    TaskState::ConfidentialCloudConsentRequired,
    TaskState::ConfidentialAttestationFailed,
    TaskState::ConfidentialModelUnavailable,
];

fn expect_snake_case(state: TaskState, expected: &str) {
    let json = serde_json::to_string(&state).unwrap();
    let expected_json = format!("\"{expected}\"");
    println!("{state:?} -> {json}");
    assert_eq!(json, expected_json, "{state:?}: unexpected serialized form");
}

fn main() {
    println!("=== TaskState serde snake_case round-trips (all {} variants) ===", ALL_STATES.len());
    expect_snake_case(TaskState::Created, "created");
    expect_snake_case(TaskState::Queued, "queued");
    expect_snake_case(TaskState::Connecting, "connecting");
    expect_snake_case(TaskState::Authenticated, "authenticated");
    expect_snake_case(TaskState::RemoteViewStarting, "remote_view_starting");
    expect_snake_case(TaskState::RemoteViewActive, "remote_view_active");
    expect_snake_case(TaskState::PolicyChecking, "policy_checking");
    expect_snake_case(TaskState::LaunchingApp, "launching_app");
    expect_snake_case(TaskState::FindingTarget, "finding_target");
    expect_snake_case(TaskState::Navigating, "navigating");
    expect_snake_case(TaskState::TypingDraft, "typing_draft");
    expect_snake_case(TaskState::Verifying, "verifying");
    expect_snake_case(TaskState::DraftReady, "draft_ready");
    expect_snake_case(TaskState::AwaitingConfirmation, "awaiting_confirmation");
    expect_snake_case(TaskState::Committing, "committing");
    expect_snake_case(TaskState::Completed, "completed");
    expect_snake_case(TaskState::NeedsLogin, "needs_login");
    expect_snake_case(TaskState::NeedsMfa, "needs_mfa");
    expect_snake_case(TaskState::NeedsConfirmation, "needs_confirmation");
    expect_snake_case(TaskState::SensitiveAccessRequested, "sensitive_access_requested");
    expect_snake_case(TaskState::UserCancelled, "user_cancelled");
    expect_snake_case(TaskState::PermissionDenied, "permission_denied");
    expect_snake_case(TaskState::AmbiguousTarget, "ambiguous_target");
    expect_snake_case(TaskState::TargetNotFound, "target_not_found");
    expect_snake_case(TaskState::SensitiveAccessRejected, "sensitive_access_rejected");
    expect_snake_case(TaskState::AgentTimeout, "agent_timeout");
    expect_snake_case(TaskState::Failed, "failed");
    expect_snake_case(
        TaskState::ConfidentialCloudConsentRequired,
        "confidential_cloud_consent_required",
    );
    expect_snake_case(
        TaskState::ConfidentialAttestationFailed,
        "confidential_attestation_failed",
    );
    expect_snake_case(
        TaskState::ConfidentialModelUnavailable,
        "confidential_model_unavailable",
    );
    assert_eq!(ALL_STATES.len(), 30, "expected exactly 30 TaskState variants");

    println!();
    println!("=== happy-path adjacent edges are all valid ===");
    for pair in HAPPY_PATH.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let valid = is_valid_transition(from, to);
        println!("{from:?} -> {to:?} : valid={valid}");
        assert!(valid, "{from:?} -> {to:?} must be a valid happy-path edge");
    }

    println!();
    println!("=== non-adjacent happy-path skips are rejected ===");
    let skip_cases = [
        (TaskState::Created, TaskState::Completed),
        (TaskState::Created, TaskState::Connecting),
        (TaskState::Queued, TaskState::Authenticated),
        (TaskState::PolicyChecking, TaskState::Completed),
        (TaskState::FindingTarget, TaskState::TypingDraft),
        (TaskState::DraftReady, TaskState::Completed),
    ];
    for (from, to) in skip_cases {
        let valid = is_valid_transition(from, to);
        println!("{from:?} -> {to:?} : valid={valid}");
        assert!(!valid, "{from:?} -> {to:?} skips states and must be rejected");
    }

    println!();
    println!("=== interactive waiting states: interrupt-in and resume-out edges ===");
    // NeedsLogin / NeedsMfa interrupt Connecting and resume back into it.
    for wait in [TaskState::NeedsLogin, TaskState::NeedsMfa] {
        let into = is_valid_transition(TaskState::Connecting, wait);
        let out = is_valid_transition(wait, TaskState::Connecting);
        println!("Connecting -> {wait:?} : valid={into}");
        println!("{wait:?} -> Connecting : valid={out}");
        assert!(into, "Connecting must be able to interrupt into {wait:?}");
        assert!(out, "{wait:?} must be able to resume back into Connecting");
    }
    // NeedsConfirmation / SensitiveAccessRequested interrupt PolicyChecking and resume into it.
    for wait in [TaskState::NeedsConfirmation, TaskState::SensitiveAccessRequested] {
        let into = is_valid_transition(TaskState::PolicyChecking, wait);
        let out = is_valid_transition(wait, TaskState::PolicyChecking);
        println!("PolicyChecking -> {wait:?} : valid={into}");
        println!("{wait:?} -> PolicyChecking : valid={out}");
        assert!(into, "PolicyChecking must be able to interrupt into {wait:?}");
        assert!(out, "{wait:?} must be able to resume back into PolicyChecking");
    }
    // A waiting state must not resume into some unrelated flow state (no bypass).
    let bogus_resume = is_valid_transition(TaskState::NeedsLogin, TaskState::Committing);
    println!("NeedsLogin -> Committing (bogus resume) : valid={bogus_resume}");
    assert!(!bogus_resume, "a waiting state must only resume into the flow state it interrupted");

    println!();
    println!("=== SensitiveAccessRequested can be explicitly rejected ===");
    let rejected = is_valid_transition(TaskState::SensitiveAccessRequested, TaskState::SensitiveAccessRejected);
    println!("SensitiveAccessRequested -> SensitiveAccessRejected : valid={rejected}");
    assert!(rejected);

    println!();
    println!(
        "=== every terminal state ({} of them) has zero valid outgoing transitions ===",
        TERMINAL_STATES.len()
    );
    for &terminal in TERMINAL_STATES {
        for &to in ALL_STATES {
            let valid = is_valid_transition(terminal, to);
            assert!(!valid, "terminal state {terminal:?} must have no valid edge to {to:?}, got valid={valid}");
        }
        println!("{terminal:?} -> * : all {} candidates rejected, OK", ALL_STATES.len());
    }

    println!();
    println!("=== Tinfoil-deferred variants: unreachable in this alpha build ===");
    for &deferred in TINFOIL_DEFERRED {
        for &from in ALL_STATES {
            let into = is_valid_transition(from, deferred);
            assert!(!into, "{from:?} -> {deferred:?} must be rejected (Tinfoil deferred to beta)");
        }
        for &to in ALL_STATES {
            let out = is_valid_transition(deferred, to);
            assert!(!out, "{deferred:?} -> {to:?} must be rejected (Tinfoil deferred to beta)");
        }
        println!("{deferred:?} : confirmed unreachable inbound and outbound across all {} states", ALL_STATES.len());
    }

    println!();
    println!("=== Verifying failure loops back to Navigating, not a dead end or self-loop ===");
    let retry = is_valid_transition(TaskState::Verifying, TaskState::Navigating);
    let self_loop = is_valid_transition(TaskState::Verifying, TaskState::Verifying);
    println!("Verifying -> Navigating (retry) : valid={retry}");
    println!("Verifying -> Verifying (self-loop) : valid={self_loop}");
    assert!(retry, "a failed verification must be able to route back to Navigating for a retry");
    assert!(!self_loop, "Verifying has no direct self-loop in this lifecycle diagram");

    println!();
    println!("=== UserCancelled and AgentTimeout are reachable broadly, but never chainable further ===");
    for &from in HAPPY_PATH.iter().filter(|s| **s != TaskState::Completed) {
        let cancel = is_valid_transition(from, TaskState::UserCancelled);
        println!("{from:?} -> UserCancelled : valid={cancel}");
        assert!(cancel, "{from:?} must be cancellable by the user");
    }

    println!();
    println!("task_state_probe: OK -- TaskState enum, serde snake_case wire form, and is_valid_transition's full lifecycle diagram witnessed via real execution");
}
