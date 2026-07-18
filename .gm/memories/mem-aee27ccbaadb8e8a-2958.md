---
key: mem-aee27ccbaadb8e8a-2958
ns: default
created: 1784354917188
updated: 1784354917188
---

## Aro policy wrapper (policy.rs) -- complete, committed locally, blocked only on no-remote push

Implemented holoiroh/mac-daemon/src/policy.rs (Project Aro PRD 7.3/9, the central trust mechanism, real interception LOGIC per P0-7, not a prompt string):
- ActionClass 6-class enum, repr(u8) pinned to PRD ordinals (Observe=0..SensitiveTarget=5), ordinal()/label().
- classify(&ProposedAction,&SensitiveCategories)->ActionClass: total match over typed ActionVerb{Observe,Click{ClickTarget::Navigation|CommitButton|AuthControl},Type{TypeTarget::DraftBody|CredentialField},Navigate,Commit{CommitKind}}. Class-5 reuses sensitive_categories.rs classify() (no app list duplicated). Rule1 sensitive-target dominance except pure Observe.
- PolicyDecision{Allow,PauseForInputRequest{PauseKind::Credential|Mfa},RequireScopedConfirmation{commit,expires_in_secs},RequireSensitiveApproval{category_id},Reject{reason}} + permits_immediate_execution() (only Allow==go).
- decide(class,cats,category_id) = PRD 9 table: 0-2 Allow; 3 PauseForInputRequest (never executes, no default-allow fall-through, total match); 4 RequireScopedConfirmation{60=SCOPED_CONFIRMATION_TTL_SECS} default-reject-on-timeout; 5 find_by_id().setting -> AlwaysAsk=>RequireSensitiveApproval, AlwaysAllow=>Allow, HardBlock=>Reject; class-5 lookup miss fails CLOSED to approval. decide_for(&action,&cats) threads real CommitKind into class-4 without changing variant.

NO executor.rs exists in this daemon (confirmed via codesearch) -> policy.rs is STANDALONE with WIRING const + docs documenting exact gate point: control_channel::ProtocolHandler::accept read loop immediately before self.bridge.handle_message(control_message).await, gated on permits_immediate_execution (hard code gate, not prompt). Registered pub mod policy in lib.rs + mod policy in main.rs (module #![allow(dead_code)], same not-yet-wired pattern as sensitive_categories/task_state).

Note: audit_log::ActionClass (coarse wire-kind Prompt/VoiceTranscript/Stop, imported by control_channel) is a DIFFERENT type from policy::ActionClass (PRD-9 safety taxonomy) -- separate modules, no collision, documented in policy.rs module doc.

Witnessed via cargo run --example policy_probe (examples/policy_probe.rs; no test files, cargo test runs 0 -- repo no-unit-tests rule): all 6 classes, full decision table, all 3 class-5 settings, fail-closed, decide_for threading, and PRD ROW 16a ADVERSARIAL ZERO-SEND -- a real ActionVerb::Commit{Send} classifies ExternalCommitment(4) and gets RequireScopedConfirmation{Send,60}, never Allow (asserted 3 ways). cargo build --workspace warning-clean (grep -c warning ==0, forced rebuild).

Committed LOCALLY as fc35625 on branch worktree-wf_2048728e-92e-2. Could NOT push/reach gm COMPLETE: git_push gate_denied 'fatal: origin does not appear to be a git repository', git remote -v empty repo-wide -- same recurring external blocker as mem-918f7f965fd8cc8c-723 et al, requires user to add a remote.
