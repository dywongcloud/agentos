//! Manual, run-by-hand probe: exercises the real `policy` module -- the Aro
//! policy wrapper (Project Aro PRD §7.3 / §9, the central trust mechanism) --
//! against real, constructed `ProposedAction`s, printing real output.
//! Witnesses the classifier + decision function this pass adds, following this
//! repo's no-unit-tests rule (real execution, witnessed by actually running it
//! and reading the output, same pattern as `sensitive_categories_probe.rs`).
//!
//! ## What this witnesses
//!
//! - **Every one of the 6 PRD §9 action classes** is produced by
//!   `policy::classify` from a real `ProposedAction`, and pinned to its exact
//!   PRD ordinal (Observe=0 … SensitiveTarget=5).
//! - **The full PRD §9 decision table** via `policy::decide` /
//!   `policy::decide_for`: classes 0-2 → Allow; class 3 → PauseForInputRequest
//!   (never executes); class 4 → RequireScopedConfirmation (60s, default reject
//!   on timeout); class 5 → RequireSensitiveApproval by default, Allow under
//!   AlwaysAllow, Reject under HardBlock.
//! - **PRD acceptance test row 16a (adversarial "zero-send")**: a class-4
//!   `send` action is classified class-4 and gets RequireScopedConfirmation --
//!   *never* Allow. Asserted directly against a real class-4 action input.
//! - The class-5 sensitive-target check really **reuses** the
//!   `sensitive_categories` data model (a click into `com.1password.1password`
//!   classifies class-5), and honors the user's per-category
//!   AlwaysAsk/AlwaysAllow/HardBlock setting.
//!
//! This probe does **not** and cannot witness a live interception point gating
//! a real agent action mid-turn -- no such point exists in this daemon yet
//! (`holo_bridge` forwards whole prompts to `holo serve`; the per-action
//! think/act loop runs server-side). See `policy.rs`'s module doc and the
//! `WIRING` constant this probe prints for the exact, documented wiring point
//! the follow-up row will use.
//!
//! Run with `cargo run --example policy_probe`.

use holoiroh_daemon::policy::{
    self, ActionClass, ActionVerb, ClickTarget, CommitKind, PauseKind, PolicyDecision,
    ProposedAction, TypeTarget, SCOPED_CONFIRMATION_TTL_SECS, WIRING,
};
use holoiroh_daemon::sensitive_categories::{CategorySetting, SensitiveCategories};

fn main() {
    let categories = SensitiveCategories::default_categories();

    println!("=== classify: every PRD §9 class is produced, pinned to its exact ordinal ===");

    // Class 0 -- Observe.
    let observe = ProposedAction::new(ActionVerb::Observe, "read the screen");
    let c0 = policy::classify(&observe, &categories);
    println!("  observe screen                         -> {:?} (ordinal {})", c0, c0.ordinal());
    assert_eq!(c0, ActionClass::Observe);
    assert_eq!(c0.ordinal(), 0);

    // Class 1 -- Navigate (both the Navigate verb and a navigational click).
    let nav_verb = ProposedAction::new(ActionVerb::Navigate, "tab to next field");
    let nav_click = ProposedAction::new(
        ActionVerb::Click { target: ClickTarget::Navigation },
        "click a plain link",
    );
    let c1a = policy::classify(&nav_verb, &categories);
    let c1b = policy::classify(&nav_click, &categories);
    println!("  navigate (keyboard)                    -> {:?} (ordinal {})", c1a, c1a.ordinal());
    println!("  click navigational control             -> {:?} (ordinal {})", c1b, c1b.ordinal());
    assert_eq!(c1a, ActionClass::Navigate);
    assert_eq!(c1b, ActionClass::Navigate);
    assert_eq!(c1a.ordinal(), 1);

    // Class 2 -- Draft.
    let draft = ProposedAction::new(
        ActionVerb::Type { into: TypeTarget::DraftBody },
        "type the email body",
    );
    let c2 = policy::classify(&draft, &categories);
    println!("  type into draft body                   -> {:?} (ordinal {})", c2, c2.ordinal());
    assert_eq!(c2, ActionClass::Draft);
    assert_eq!(c2.ordinal(), 2);

    // Class 3 -- SensitiveTransition (credential field, and an auth-control click).
    let cred = ProposedAction::new(
        ActionVerb::Type { into: TypeTarget::CredentialField },
        "type into the password field",
    );
    let signin = ProposedAction::new(
        ActionVerb::Click { target: ClickTarget::AuthControl },
        "click Sign in",
    );
    let c3a = policy::classify(&cred, &categories);
    let c3b = policy::classify(&signin, &categories);
    println!("  type into credential field             -> {:?} (ordinal {})", c3a, c3a.ordinal());
    println!("  click auth control (sign in)           -> {:?} (ordinal {})", c3b, c3b.ordinal());
    assert_eq!(c3a, ActionClass::SensitiveTransition);
    assert_eq!(c3b, ActionClass::SensitiveTransition);
    assert_eq!(c3a.ordinal(), 3);

    // Class 4 -- ExternalCommitment (a Commit verb, and a commit-button click).
    let send_commit = ProposedAction::new(
        ActionVerb::Commit { kind: CommitKind::Send },
        "send the email",
    );
    let submit_click = ProposedAction::new(
        ActionVerb::Click { target: ClickTarget::CommitButton { kind: CommitKind::Submit } },
        "click Submit",
    );
    let c4a = policy::classify(&send_commit, &categories);
    let c4b = policy::classify(&submit_click, &categories);
    println!("  commit: send                           -> {:?} (ordinal {})", c4a, c4a.ordinal());
    println!("  click commit button (submit)           -> {:?} (ordinal {})", c4b, c4b.ordinal());
    assert_eq!(c4a, ActionClass::ExternalCommitment);
    assert_eq!(c4b, ActionClass::ExternalCommitment);
    assert_eq!(c4a.ordinal(), 4);

    // Class 5 -- SensitiveTarget (a click INTO a sensitive app, reusing the
    // sensitive_categories data model).
    let into_1password = ProposedAction::in_app(
        ActionVerb::Click { target: ClickTarget::Navigation },
        "com.1password.1password",
        "click an entry in 1Password",
    );
    let c5 = policy::classify(&into_1password, &categories);
    println!("  click into 1Password (sensitive app)   -> {:?} (ordinal {})", c5, c5.ordinal());
    assert_eq!(c5, ActionClass::SensitiveTarget);
    assert_eq!(c5.ordinal(), 5);

    // Ordinals are the exact PRD numbering, in order.
    assert_eq!(
        [
            ActionClass::Observe.ordinal(),
            ActionClass::Navigate.ordinal(),
            ActionClass::Draft.ordinal(),
            ActionClass::SensitiveTransition.ordinal(),
            ActionClass::ExternalCommitment.ordinal(),
            ActionClass::SensitiveTarget.ordinal(),
        ],
        [0, 1, 2, 3, 4, 5]
    );

    // Pure observation of a sensitive app is NOT a sensitive access (class 0,
    // not class 5) -- looking changes nothing; the class-5 gate is about
    // acting into the app.
    let observe_1password = ProposedAction::in_app(
        ActionVerb::Observe,
        "com.1password.1password",
        "screenshot 1Password's window",
    );
    let c_obs5 = policy::classify(&observe_1password, &categories);
    println!("  observe 1Password (read-only)          -> {:?} (ordinal {})", c_obs5, c_obs5.ordinal());
    assert_eq!(c_obs5, ActionClass::Observe, "reading a sensitive app is not a class-5 access");

    println!();
    println!("=== decide: the full PRD §9 decision table ===");

    // Classes 0-2 -> Allow.
    for (label, class) in [
        ("observe (0)", ActionClass::Observe),
        ("navigate (1)", ActionClass::Navigate),
        ("draft (2)", ActionClass::Draft),
    ] {
        let d = policy::decide(class, &categories, None);
        println!("  {label:<14} -> {:?}", d);
        assert_eq!(d, PolicyDecision::Allow, "classes 0-2 are allowed by default");
        assert!(d.permits_immediate_execution());
    }

    // Class 3 -> PauseForInputRequest; NEVER executes.
    let d3 = policy::decide(ActionClass::SensitiveTransition, &categories, None);
    println!("  sensitive_transition (3) -> {:?}", d3);
    assert_eq!(d3, PolicyDecision::PauseForInputRequest { kind: PauseKind::Credential });
    assert!(!d3.permits_immediate_execution(), "class 3 must never execute directly");

    // Class 4 -> RequireScopedConfirmation, 60s, default reject on timeout.
    let d4 = policy::decide(ActionClass::ExternalCommitment, &categories, None);
    println!("  external_commitment (4)  -> {:?}", d4);
    match &d4 {
        PolicyDecision::RequireScopedConfirmation { expires_in_secs, .. } => {
            assert_eq!(*expires_in_secs, SCOPED_CONFIRMATION_TTL_SECS);
            assert_eq!(*expires_in_secs, 60);
        }
        other => panic!("class 4 must require a scoped confirmation, got {other:?}"),
    }
    assert!(!d4.permits_immediate_execution(), "class 4 must never execute directly");

    // Class 5 -> RequireSensitiveApproval by default (AlwaysAsk).
    let d5_ask = policy::decide(
        ActionClass::SensitiveTarget,
        &categories,
        Some("password_managers"),
    );
    println!("  sensitive_target (5), AlwaysAsk  -> {:?}", d5_ask);
    assert_eq!(
        d5_ask,
        PolicyDecision::RequireSensitiveApproval { category_id: "password_managers".to_string() }
    );
    assert!(!d5_ask.permits_immediate_execution());

    // Class 5 with AlwaysAllow -> Allow; with HardBlock -> Reject.
    let mut cats_allow = SensitiveCategories::default_categories();
    cats_allow.find_by_id_mut("password_managers").unwrap().setting = CategorySetting::AlwaysAllow;
    let d5_allow = policy::decide(ActionClass::SensitiveTarget, &cats_allow, Some("password_managers"));
    println!("  sensitive_target (5), AlwaysAllow -> {:?}", d5_allow);
    assert_eq!(d5_allow, PolicyDecision::Allow);
    assert!(d5_allow.permits_immediate_execution());

    let mut cats_block = SensitiveCategories::default_categories();
    cats_block.find_by_id_mut("banking_brokerage").unwrap().setting = CategorySetting::HardBlock;
    let d5_block = policy::decide(ActionClass::SensitiveTarget, &cats_block, Some("banking_brokerage"));
    println!("  sensitive_target (5), HardBlock   -> {:?}", d5_block);
    assert!(matches!(d5_block, PolicyDecision::Reject { .. }));
    assert!(!d5_block.permits_immediate_execution());

    // Fail-closed: a class-5 decision with an unresolvable category id must
    // NOT become Allow -- it defaults to the AlwaysAsk approval gate.
    let d5_miss = policy::decide(ActionClass::SensitiveTarget, &categories, Some("no_such_category"));
    println!("  sensitive_target (5), bad cat id  -> {:?} (fails closed to approval)", d5_miss);
    assert!(
        matches!(d5_miss, PolicyDecision::RequireSensitiveApproval { .. }),
        "a class-5 lookup miss must fail closed, never Allow"
    );

    println!();
    println!("=== decide_for: end-to-end (classify + decide) with real CommitKind threading ===");
    // A commit-button click threads the real CommitKind (Payment) into the
    // scoped confirmation, without ever changing the decision away from
    // RequireScopedConfirmation.
    let pay = ProposedAction::new(
        ActionVerb::Click { target: ClickTarget::CommitButton { kind: CommitKind::Payment } },
        "confirm the $340 payment",
    );
    let d_pay = policy::decide_for(&pay, &categories);
    println!("  click Pay button -> {:?}", d_pay);
    assert_eq!(
        d_pay,
        PolicyDecision::RequireScopedConfirmation {
            commit: CommitKind::Payment,
            expires_in_secs: 60,
        }
    );

    println!();
    println!("=== PRD ACCEPTANCE TEST (row 16a, adversarial zero-send) ===");
    println!("A class-4 send action MUST classify class-4 and get RequireScopedConfirmation, never Allow.");
    // The exact adversarial input: an agent proposing to SEND. Built as a real
    // class-4 action, run through the real classifier and the real decision
    // function -- no mocking.
    let adversarial_send = ProposedAction::new(
        ActionVerb::Commit { kind: CommitKind::Send },
        "send email to investors@example.com",
    );
    let send_class = policy::classify(&adversarial_send, &categories);
    let send_decision = policy::decide_for(&adversarial_send, &categories);
    println!("  input:    {:?}", adversarial_send);
    println!("  class:    {:?} (ordinal {})", send_class, send_class.ordinal());
    println!("  decision: {:?}", send_decision);

    // The invariant, asserted three independent ways.
    assert_eq!(send_class, ActionClass::ExternalCommitment, "a send must be class 4");
    assert_eq!(send_class.ordinal(), 4);
    assert_ne!(send_decision, PolicyDecision::Allow, "a class-4 send must NEVER be Allow (zero-send)");
    assert!(
        !send_decision.permits_immediate_execution(),
        "a class-4 send must NEVER permit immediate execution (zero-send)"
    );
    match &send_decision {
        PolicyDecision::RequireScopedConfirmation { commit, expires_in_secs } => {
            assert_eq!(*commit, CommitKind::Send);
            assert_eq!(*expires_in_secs, 60, "scoped confirmation expires after 60s, default reject on timeout");
        }
        other => panic!("zero-send invariant violated: class-4 send got {other:?}, expected RequireScopedConfirmation"),
    }
    println!("  ZERO-SEND INVARIANT HELD: class-4 send -> RequireScopedConfirmation (60s), never Allow.");

    println!();
    println!("=== documented wiring point (for the follow-up interception row) ===");
    println!("{WIRING}");
    assert!(WIRING.contains("handle_message"), "wiring note must name the exact dispatch it gates");
    assert!(WIRING.contains("permits_immediate_execution"), "wiring note must name the go/no-go predicate");

    println!();
    println!("policy_probe: OK -- classifier, decision table, class-5 data-model reuse, and the PRD");
    println!("row-16a adversarial zero-send invariant all witnessed via real execution.");
    println!();
    println!(
        "NOTE: this probe witnesses the policy engine only. No live interception point consults it \
         yet -- see policy.rs's module doc and the WIRING constant above for the exact, documented \
         wiring point the follow-up row will use."
    );
}
