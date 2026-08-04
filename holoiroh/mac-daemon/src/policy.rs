//! Implements the Project Aro policy decision mapping.
//! The Product Requirements Document (PRD) specifies this mapping in §7.3 and §9.
//!
//! This module is the daemon-side classifier for typed desktop actions.
//! [`crate::action_executor`] calls it before each typed action.
//! The legacy Holo backend remains opaque and does not use this path.
//!
//! `examples/policy_probe.rs` exercises this standalone component.
//! [`crate::sensitive_categories`] and [`crate::limits`] have the same standalone status.
//!
//! ## Implemented decision mapping
//!
//! The component models the PRD §9 six-class taxonomy.
//! [`classify`] maps each available typed action to one [`ActionClass`].
//! [`decide`] maps that class and applicable category configuration to one [`PolicyDecision`].
//! Only [`PolicyDecision::Allow`] permits immediate execution at a future interception point.
//!
//! The mapping does not itself pause, request approval, reject runtime work, or dispatch an action.
//! A caller with a per-action stream must implement those effects.
//!
//! ## Unavailable per-action interception
//!
//! A future executor can call [`decide_for`] between its proposal and execution steps.
//! [`WIRING`] describes that intended integration point and its required behavior.
//! This description is not a claim that the live path uses the component.
//!
//! ## Relationship to [`crate::audit_log::ActionClass`]
//!
//! `audit_log::ActionClass` records the wire-message kind that started a task.
//! Its values are `Prompt`, `VoiceTranscript`, and `Stop`.
//! This module's [`ActionClass`] classifies one proposed computer-use action.
//! The separate enums represent independent audit and policy concepts.
//!
//! ## Why `#![allow(dead_code)]`
//!
//! `main.rs` and `control_channel.rs` do not call this module.
//! `pub mod policy;` keeps the standalone API compiled and available to the probe.
//! The module-level attribute avoids repeated `#[allow(dead_code)]` attributes.
#![allow(dead_code)]

use crate::sensitive_categories::{CategorySetting, SensitiveCategories};

/// Sets the class-4 scoped-confirmation lifetime to 60 seconds.
/// PRD §9 requires rejection when this lifetime expires.
/// [`decide`] stores this value in `expires_in_secs`.
pub const SCOPED_CONFIRMATION_TTL_SECS: u64 = 60;

/// Represents PRD §9's six action classes.
///
/// `#[repr(u8)]` and explicit values preserve the PRD ordinals from 0 through 5.
/// Reordering variants cannot silently change these ordinals.
/// [`classify`] selects a class from an available typed action.
/// [`decide`] maps a class and applicable category configuration to a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ActionClass {
    /// Class 0 observes without changing state.
    /// Examples include reading, screenshots, scrolling, and tooltip reveals.
    /// The mapping allows this class by default.
    Observe = 0,
    /// Class 1 changes the visible context without making a commitment.
    /// Examples include tabs, menus, non-committing links, and field focus.
    /// The mapping allows this class by default.
    Navigate = 1,
    /// Class 2 creates reversible local content that has not been committed.
    /// Examples include message bodies, document bodies, and unsubmitted forms.
    /// The mapping allows this class by default.
    /// Send, submit, and pay operations belong to class 4.
    Draft = 2,
    /// Class 3 crosses a credential or authentication boundary.
    /// Examples include login, password, multi-factor authentication, and authorization controls.
    /// PRD §9 maps this class to [`PolicyDecision::PauseForInputRequest`].
    /// A future caller must obtain credential input outside the agent context.
    SensitiveTransition = 3,
    /// Class 4 makes an externally visible or hard-to-reverse commitment.
    /// Examples include send, submit, publish, purchase, payment, transfer, and deletion operations.
    /// The mapping requires a distinct scoped confirmation.
    /// The confirmation expires after [`SCOPED_CONFIRMATION_TTL_SECS`].
    /// Expiration produces rejection.
    /// PRD row 16a requires a `send` action to map here, never to `Allow`.
    ExternalCommitment = 4,
    /// Class 5 targets a category in [`crate::sensitive_categories`].
    /// Examples include password managers, banking, brokerage, health, and system settings.
    /// Other examples include security settings and administration consoles.
    /// [`CategorySetting::AlwaysAsk`] maps to per-access approval.
    /// [`CategorySetting::AlwaysAllow`] maps to [`PolicyDecision::Allow`].
    /// [`CategorySetting::HardBlock`] maps to [`PolicyDecision::Reject`].
    SensitiveTarget = 5,
}

impl ActionClass {
    /// Returns the stable PRD ordinal from 0 through 5 as a `u8`.
    /// This named method makes log and probe call sites show their intent.
    pub fn ordinal(self) -> u8 {
        self as u8
    }

    /// Returns the PRD short name in snake case.
    /// Logging, diagnostics, and the probe use this human-readable label.
    /// The label is not a Serde wire representation.
    pub fn label(self) -> &'static str {
        match self {
            ActionClass::Observe => "observe",
            ActionClass::Navigate => "navigate",
            ActionClass::Draft => "draft",
            ActionClass::SensitiveTransition => "sensitive_transition",
            ActionClass::ExternalCommitment => "external_commitment",
            ActionClass::SensitiveTarget => "sensitive_target",
        }
    }
}

/// Represents the operation in an available typed action.
///
/// The closed enum lets [`classify`] use a total `match` without inspecting free text.
/// A future per-action integration must translate its concrete action vocabulary into these values.
/// The live opaque backend does not currently provide such actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionVerb {
    /// Observes through screenshots, screen reading, scrolling, or hovering.
    Observe,
    /// Clicks a target described by [`ClickTarget`].
    /// The target determines the class.
    Click { target: ClickTarget },
    /// Types into a semantic destination described by [`TypeTarget`].
    /// The destination determines the class.
    /// The mapping does not inspect typed characters.
    /// Credential values must not reach this component.
    Type { into: TypeTarget },
    /// Moves focus or navigates by keyboard without making a commitment.
    Navigate,
    /// Represents an explicit external commitment with its [`CommitKind`].
    /// The mapping assigns this verb to class 4 for every target.
    Commit { kind: CommitKind },
}

/// Identifies the semantic target of [`ActionVerb::Click`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickTarget {
    /// Identifies a non-committing tab, menu item, link, or disclosure control.
    /// The mapping assigns this target to class 1.
    Navigation,
    /// Identifies a send, submit, post, publish, payment, purchase, or deletion control.
    /// The mapping assigns this target to class 4.
    /// `kind` supplies context for a future confirmation prompt.
    CommitButton { kind: CommitKind },
    /// Identifies a login, authorization, unlock, or multi-factor approval control.
    /// The mapping assigns this target to class 3.
    /// A future caller must not click it without satisfying the mapped gate.
    AuthControl,
}

/// Identifies the semantic destination of [`ActionVerb::Type`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeTarget {
    /// Identifies a reversible message, document, form, or search draft.
    /// The mapping assigns this destination to class 2.
    DraftBody,
    /// Identifies a password, application programming interface (API) key, secret, or one-time-code field.
    /// The mapping assigns this destination to class 3.
    /// A future caller must collect the value outside the agent context.
    CredentialField,
}

/// Identifies the external commitment represented by a class-4 action.
/// A future confirmation prompt can use it to describe the specific commitment.
/// Examples include "send this email" and "confirm this $340 payment".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKind {
    /// Sends a message, email, or direct message.
    Send,
    /// Submits a form, post, or publication.
    Submit,
    /// Confirms a purchase, payment, or money transfer.
    Payment,
    /// Performs a destructive or irreversible operation, such as deletion or wiping.
    Destructive,
}

impl CommitKind {
    /// Returns a short phrase for human-readable confirmation context.
    pub fn label(self) -> &'static str {
        match self {
            CommitKind::Send => "send",
            CommitKind::Submit => "submit",
            CommitKind::Payment => "payment",
            CommitKind::Destructive => "destructive action",
        }
    }
}

/// Contains an available typed action for classification.
///
/// `verb` identifies the operation.
/// `target_bundle_id` identifies the macOS app when attribution is available.
/// Its value is the app's bundle identifier (ID).
/// [`SensitiveCategories::classify`] performs the class-5 membership test.
/// This component does not duplicate the sensitive-app list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedAction {
    /// What the action does.
    pub verb: ActionVerb,
    /// Identifies the target app's bundle ID when attribution is available.
    /// `None` indicates that per-app attribution is unavailable.
    /// The daemon commonly uses `None` because `AppCategory` contains only `Desktop`.
    /// With `None`, classification uses only the verb and its semantic target.
    pub target_bundle_id: Option<String>,
    /// Supplies human-readable context for traces and future approval prompts.
    /// This field must never contain a credential value.
    /// This restriction also applies to `Type { into: CredentialField }`.
    /// `control_channel::ServerMessage::InputRequest` documents the same boundary.
    pub description: String,
}

impl ProposedAction {
    /// Creates an action without app attribution.
    pub fn new(verb: ActionVerb, description: impl Into<String>) -> Self {
        ProposedAction {
            verb,
            target_bundle_id: None,
            description: description.into(),
        }
    }

    /// Creates an action with a known target-app bundle ID.
    pub fn in_app(
        verb: ActionVerb,
        bundle_id: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        ProposedAction {
            verb,
            target_bundle_id: Some(bundle_id.into()),
            description: description.into(),
        }
    }
}

/// Maps an available typed action to its PRD §9 class.
///
/// The function first checks a non-observation action's `target_bundle_id`.
/// A bundle ID in [`SensitiveCategories`] produces [`ActionClass::SensitiveTarget`].
/// [`ActionVerb::Observe`] remains [`ActionClass::Observe`] for a sensitive target.
///
/// Otherwise, the verb and target determine the class:
///
/// - `Observe` maps to class 0.
/// - `Navigate` maps to class 1.
/// - A navigation click maps to class 1.
/// - Draft-body typing maps to class 2.
/// - An authentication click maps to class 3.
/// - Credential-field typing maps to class 3.
/// - A commit button or `Commit` verb maps to class 4.
///
/// The function is total and has no side effects.
/// The same input and category data produce the same class.
pub fn classify(action: &ProposedAction, categories: &SensitiveCategories) -> ActionClass {
    // Rule 1: sensitive-target dominance (reusing the class-5 data model),
    // except for pure observation, which never "accesses" the sensitive app.
    if !matches!(action.verb, ActionVerb::Observe) {
        if let Some(bundle_id) = action.target_bundle_id.as_deref() {
            if categories.classify(bundle_id).is_some() {
                return ActionClass::SensitiveTarget;
            }
        }
    }

    // Rule 2: verb-driven classification (total match).
    match &action.verb {
        ActionVerb::Observe => ActionClass::Observe,
        ActionVerb::Navigate => ActionClass::Navigate,
        ActionVerb::Click { target } => match target {
            ClickTarget::Navigation => ActionClass::Navigate,
            ClickTarget::AuthControl => ActionClass::SensitiveTransition,
            ClickTarget::CommitButton { .. } => ActionClass::ExternalCommitment,
        },
        ActionVerb::Type { into } => match into {
            TypeTarget::DraftBody => ActionClass::Draft,
            TypeTarget::CredentialField => ActionClass::SensitiveTransition,
        },
        ActionVerb::Commit { .. } => ActionClass::ExternalCommitment,
    }
}

/// Identifies the credential-boundary request associated with a class-3 decision.
///
/// The values correspond to the `credential` and `mfa` input-request kinds.
/// This transport-independent type does not raise an `input_request`.
/// A future caller can translate it to `control_channel::InputRequestKind`.
/// PRD §9 prohibits credentials from entering the agent context.
/// `PROTOCOL.md` prohibits credentials from traveling on the control channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseKind {
    /// Identifies the `credential` request kind for a password, API key, or secret.
    Credential,
    /// Identifies the `mfa` request kind for a multi-factor code or approval.
    Mfa,
}

/// Represents the result of the implemented policy decision mapping.
///
/// [`PolicyDecision::Allow`] permits immediate execution at a future interception point.
/// Each other value describes a gate or rejection that a future caller must implement.
/// This enum does not itself enforce, pause, approve, reject, or execute an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Permits immediate execution at a future interception point.
    /// Classes 0 through 2 map here by default.
    /// Class 5 also maps here for [`CategorySetting::AlwaysAllow`].
    Allow,
    /// Requires a future caller to pause for out-of-band credential input.
    /// Class 3 maps here.
    /// The caller must not execute the proposed credential action.
    /// Credential values must not enter the agent context.
    /// `control_channel` uses its safe-pause path when an input request expires.
    PauseForInputRequest { kind: PauseKind },
    /// Requires a distinct, single-use confirmation for a class-4 action.
    /// `expires_in_secs` specifies the confirmation lifetime.
    /// The default lifetime is 60 seconds.
    /// Expiration rejects the action.
    /// `commit` identifies the external commitment for prompt context.
    RequireScopedConfirmation {
        commit: CommitKind,
        expires_in_secs: u64,
    },
    /// Requires per-access approval for a class-5 sensitive app.
    /// [`CategorySetting::AlwaysAsk`] maps here by default.
    /// `category_id` identifies the matched sensitive category.
    RequireSensitiveApproval { category_id: String },
    /// Requires a future caller to refuse the action.
    /// [`CategorySetting::HardBlock`] produces this decision for class 5.
    /// The implemented mapping produces this decision only for class 5.
    /// The variant can also represent a future explicitly forbidden action.
    /// `reason` supplies human-readable user and log context.
    Reject { reason: String },
}

impl PolicyDecision {
    /// Reports whether the decision is exactly [`Self::Allow`].
    ///
    /// A future interception point can use this result before execution.
    pub fn permits_immediate_execution(&self) -> bool {
        matches!(self, PolicyDecision::Allow)
    }

    /// Returns a short label for logging and diagnostics.
    pub fn label(&self) -> &'static str {
        match self {
            PolicyDecision::Allow => "allow",
            PolicyDecision::PauseForInputRequest { .. } => "pause_for_input_request",
            PolicyDecision::RequireScopedConfirmation { .. } => "require_scoped_confirmation",
            PolicyDecision::RequireSensitiveApproval { .. } => "require_sensitive_approval",
            PolicyDecision::Reject { .. } => "reject",
        }
    }
}

/// Maps an [`ActionClass`] to the PRD §9 policy decision.
///
/// `categories` supplies the user's class-5 category settings.
/// `category_id` identifies the matched category for class 5.
/// The function ignores `category_id` for classes 0 through 4.
///
/// The function implements this mapping:
///
/// | Class | Name | Decision |
/// |---|---|---|
/// | 0 | Observe | [`PolicyDecision::Allow`] |
/// | 1 | Navigate | [`PolicyDecision::Allow`] |
/// | 2 | Draft | [`PolicyDecision::Allow`] |
/// | 3 | SensitiveTransition | [`PolicyDecision::PauseForInputRequest`] |
/// | 4 | ExternalCommitment | [`PolicyDecision::RequireScopedConfirmation`] |
/// | 5 | SensitiveTarget | The matching category setting controls the decision. |
///
/// Class 3 never returns [`PolicyDecision::Allow`].
/// Class 4 never returns [`PolicyDecision::Allow`].
/// Class 4 uses a 60-second confirmation lifetime and rejects on timeout.
/// Class 5 defaults to [`CategorySetting::AlwaysAsk`] when category lookup fails.
///
/// This function returns a decision value only.
/// It does not intercept an action or enforce the returned decision.
/// The live opaque backend does not provide the daemon with a per-action stream.
///
/// A future per-action executor must call this mapping before action execution.
/// It must execute immediately only for [`PolicyDecision::Allow`].
/// It must complete each other gate before execution, or not execute the action.
/// See [`WIRING`] for the proposed integration point.

pub fn decide(
    class: ActionClass,
    categories: &SensitiveCategories,
    category_id: Option<&str>,
) -> PolicyDecision {
    match class {
        // Classes 0-2: allowed by default.
        ActionClass::Observe | ActionClass::Navigate | ActionClass::Draft => PolicyDecision::Allow,

        // Class 3: pause and fire an input_request. Never executes; the
        // credential/MFA value is entered out-of-band and never passes
        // through the agent context. Credential vs. MFA is not derivable from
        // the class alone, so the default is `Credential` (a plain
        // credential/auth boundary); a caller that knows the boundary is
        // specifically an MFA prompt can refine the raised kind at the wiring
        // point. Either way it is a PauseForInputRequest, never an execute.
        ActionClass::SensitiveTransition => PolicyDecision::PauseForInputRequest {
            kind: PauseKind::Credential,
        },

        // Class 4: distinct scoped confirmation, 60s, default reject on
        // timeout. There is NO branch here that returns Allow for a class-4
        // action -- this is the row-16a adversarial-zero-send invariant, held
        // by construction. The concrete CommitKind is not recoverable from
        // the class ordinal alone (it was on the ProposedAction), so the
        // table stamps `Submit` as a neutral placeholder; the wiring point
        // that has the ProposedAction in hand passes the real CommitKind
        // through (see `decide_for`). Either way the DECISION is
        // RequireScopedConfirmation, which is what the invariant is about.
        ActionClass::ExternalCommitment => PolicyDecision::RequireScopedConfirmation {
            commit: CommitKind::Submit,
            expires_in_secs: SCOPED_CONFIRMATION_TTL_SECS,
        },

        // Class 5: per-access approval by default, unless the user's
        // per-category setting overrides to AlwaysAllow (Allow) or HardBlock
        // (Reject). The setting is looked up from the reused class-5 data
        // model via the matched category id.
        ActionClass::SensitiveTarget => {
            let setting = category_id
                .and_then(|id| categories.find_by_id(id))
                .map(|c| c.setting)
                // A class-5 classification with no resolvable category id is a
                // caller error (classify() only returns class 5 when a
                // category matched), but fail *closed* to the default AlwaysAsk
                // rather than allowing -- a sensitive action must never become
                // an Allow through a lookup miss.
                .unwrap_or(CategorySetting::AlwaysAsk);
            match setting {
                CategorySetting::AlwaysAsk => PolicyDecision::RequireSensitiveApproval {
                    category_id: category_id.unwrap_or("").to_string(),
                },
                CategorySetting::AlwaysAllow => PolicyDecision::Allow,
                CategorySetting::HardBlock => PolicyDecision::Reject {
                    reason: format!(
                        "sensitive category '{}' is set to hard-block",
                        category_id.unwrap_or("<unknown>")
                    ),
                },
            }
        }
    }
}

/// Classifies an action and returns its mapped decision.
///
/// For class 5, the function passes the matched category ID to [`decide`].
/// For class 4, it preserves the action's concrete [`CommitKind`] when available.
/// Otherwise, class 4 uses [`CommitKind::Submit`].
/// The function never maps class 4 to [`PolicyDecision::Allow`].
/// It returns a value and does not intercept or execute the action.
pub fn decide_for(action: &ProposedAction, categories: &SensitiveCategories) -> PolicyDecision {
    let class = classify(action, categories);

    // For class 5, resolve the concrete matched category id from the reused
    // data model so `decide` can consult the user's per-category setting.
    let category_id = if class == ActionClass::SensitiveTarget {
        action
            .target_bundle_id
            .as_deref()
            .and_then(|b| categories.classify(b))
            .map(|c| c.id.clone())
    } else {
        None
    };

    let decision = decide(class, categories, category_id.as_deref());

    // Refine the placeholder CommitKind in a class-4 scoped confirmation with
    // the real one from the action, when the action actually carries it. This
    // never changes the decision *variant* (still RequireScopedConfirmation),
    // only its `commit` detail -- the zero-send invariant is untouched.
    match decision {
        PolicyDecision::RequireScopedConfirmation {
            expires_in_secs, ..
        } => {
            let commit = commit_kind_of(&action.verb).unwrap_or(CommitKind::Submit);
            PolicyDecision::RequireScopedConfirmation {
                commit,
                expires_in_secs,
            }
        }
        other => other,
    }
}

/// Returns the [`CommitKind`] carried by a commit verb or commit button.
/// Returns `None` for all other verbs.
fn commit_kind_of(verb: &ActionVerb) -> Option<CommitKind> {
    match verb {
        ActionVerb::Commit { kind } => Some(*kind),
        ActionVerb::Click {
            target: ClickTarget::CommitButton { kind },
        } => Some(*kind),
        _ => None,
    }
}

/// Describes a proposed integration point for a future per-action executor.
///
/// The live opaque backend does not expose individual actions to the daemon.
/// Therefore, this integration is not currently available or active.
/// The constant is probe-printable and does not perform interception.

pub const WIRING: &str = "\
policy wrapper wiring point: call policy::decide_for(&proposed_action, &categories) in \
control_channel::ProtocolHandler::accept's read loop, immediately before \
self.bridge.handle_message(control_message).await (the 'act' dispatch). Dispatch the action \
ONLY when the decision permits_immediate_execution() (== PolicyDecision::Allow); otherwise honor \
the gate: PauseForInputRequest -> raise ServerMessage::input_request and do not dispatch; \
RequireScopedConfirmation -> mint a distinct 60s single-use approval (limits::ApprovalToken), \
dispatch only if granted before expiry, default reject on timeout; RequireSensitiveApproval -> \
sensitive-access consent round trip, dispatch only on approval; Reject -> ServerMessage::error, \
do not dispatch. This is a hard code gate (PRD P0-7), never a prompt instruction. Requires a \
per-action stream this daemon does not have yet (holo_bridge forwards whole prompts to holo serve).";
