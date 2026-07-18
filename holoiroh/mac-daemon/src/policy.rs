//! The Aro policy wrapper: the central trust mechanism of Project Aro
//! (PRD §7.3 "Policy wrapper / interception layer" and §9 "Safety, Consent,
//! and Sensitive Apps").
//!
//! This module is **real interception logic, not a prompt string.** PRD row
//! P0-7 is explicit that the 6-class action taxonomy and its decision table
//! must be *enforced in the policy layer* -- a hard code gate between the
//! agent proposing an action and the runtime executing it -- and must **not**
//! be delegated to a natural-language instruction in the model's system
//! prompt (which the model can ignore, be jailbroken past, or silently drift
//! from). Everything here is a `match` over a typed [`ProposedAction`], never
//! a comparison against model-generated free text.
//!
//! ## Where this sits: between "think" and "act"
//!
//! The Aro executor runs a computer-use loop of *think* (the VLM proposes a
//! concrete next action from a screenshot + goal) and *act* (the runtime
//! actually performs the click/keystroke/navigation). This wrapper is
//! designed to sit **between** those two steps:
//!
//! ```text
//!   VLM proposes action ──▶ [ policy::classify ] ──▶ [ policy::decide ] ──▶ runtime acts
//!        (think)                    │                       │                   (act)
//!                                   ▼                       ▼
//!                            ActionClass 0..5      PolicyDecision {Allow | Pause |
//!                                                   RequireScopedConfirmation |
//!                                                   RequireSensitiveApproval | Reject}
//! ```
//!
//! Only a [`PolicyDecision::Allow`] lets the action reach the runtime
//! unchanged. Every other decision interposes a gate (pause-and-ask, a
//! scoped time-boxed confirmation, a per-access sensitive approval, or an
//! outright reject) *before* the "act" step -- the action is never performed
//! speculatively and then undone.
//!
//! ## Wiring status (honest, per this repo's convention)
//!
//! There is **no `executor.rs` / `ComputerUseExecutor` in this Rust daemon
//! yet** -- `holo_bridge` forwards each prompt straight through to
//! `holo serve`, which runs H Company's Holo3 agent server-side, and this
//! daemon never sees the individual VLM-proposed actions that agent takes.
//! So this module is a **real, standalone, exhaustively-witnessed policy
//! engine** (see `examples/policy_probe.rs`) that is not yet consulted from a
//! live interception point, exactly the same honest status
//! [`crate::sensitive_categories`] and [`crate::limits`] carry.
//!
//! The **exact wiring point**, for the follow-up task that gives this daemon
//! a real per-action stream to gate, is documented on [`decide`] and again in
//! [`WIRING`] -- it is a hard code gate, not a prompt edit.
//!
//! ## Relationship to [`crate::audit_log::ActionClass`]
//!
//! `audit_log` already has a type spelled `ActionClass`, but it is a
//! **different, coarser concept**: which *wire-message kind*
//! (`Prompt`/`VoiceTranscript`/`Stop`) started a task, for audit attribution.
//! *This* module's [`ActionClass`] is PRD §9's **6-class safety taxonomy** of
//! a single proposed computer-use action (observe / navigate / draft /
//! sensitive-transition / external-commitment / sensitive-target). The two
//! live in separate modules and never collide at a use site; this doc note
//! exists so a reader who greps `ActionClass` across the crate is not
//! confused into thinking they are the same enum. They are deliberately not
//! merged -- unifying an audit-attribution enum with a safety-gate enum would
//! conflate two orthogonal axes.
//!
//! ## Why `#![allow(dead_code)]`
//!
//! Nothing in `main.rs` / `control_channel.rs` calls into this module yet
//! (see "Wiring status" above) -- this pass adds the module and registers it
//! via `pub mod policy;` so it is compiled, reachable for the follow-up
//! interception row to call, and exercised for real by
//! `examples/policy_probe.rs`. Every item here is real, working, documented
//! public API (same status the other not-yet-wired modules carry); this
//! blanket module-level attribute avoids repeating `#[allow(dead_code)]` on
//! every item below.

#![allow(dead_code)]

use crate::sensitive_categories::{CategorySetting, SensitiveCategories};

/// The default lifetime of a class-4 scoped confirmation, in seconds
/// (PRD §9: "a distinct, scoped confirmation that expires after 60 seconds
/// and defaults to reject on timeout"). Named here so the value cited by the
/// PRD lives in exactly one place; [`decide`] stamps it onto
/// [`PolicyDecision::RequireScopedConfirmation`]'s `expires_in_secs`.
pub const SCOPED_CONFIRMATION_TTL_SECS: u64 = 60;

/// PRD §9's 6-class action taxonomy for a single proposed computer-use
/// action. The discriminants are pinned to the exact ordinals the PRD gives
/// (Observe = 0 … SensitiveTarget = 5) via `#[repr(u8)]` and explicit
/// values, so [`ActionClass as u8`] is a stable, PRD-faithful wire/log number
/// -- never left to Rust's default enum-ordering, which a later reordering of
/// the variants could silently change.
///
/// Ordering matters beyond the numbering: the classes are arranged from
/// least to most trust-sensitive, and [`decide`]'s table is monotonic in that
/// sense (a higher class never yields a *more* permissive decision than a
/// lower one). The classifier ([`classify`]) returns the class; the decision
/// function ([`decide`]) maps a class (plus, for class 5, the user's
/// per-category configuration) to a [`PolicyDecision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ActionClass {
    /// **Class 0 -- Observe.** Reading the screen, taking a screenshot,
    /// scrolling to read, moving the pointer to reveal a tooltip: anything
    /// that gathers information without changing any state. Always allowed.
    Observe = 0,
    /// **Class 1 -- Navigate.** Moving *within* an already-open, non-sensitive
    /// context: switching tabs, opening a menu, clicking a non-committing
    /// link, focusing a field. Changes what is on screen but commits nothing
    /// and crosses no trust boundary. Allowed by default.
    Navigate = 1,
    /// **Class 2 -- Draft.** Composing reversible local content that has not
    /// been sent or committed anywhere: typing into a message/document body,
    /// filling a form that has not been submitted, editing a draft. Fully
    /// reversible and local, so allowed by default -- the commitment gate is
    /// class 4, which is where "send"/"submit"/"pay" lands.
    Draft = 2,
    /// **Class 3 -- SensitiveTransition.** The action would cross a
    /// credential/authentication boundary: a login/sign-in field, a password
    /// prompt, an MFA/2FA code entry, an unlock/authorize dialog. Per PRD §9
    /// the executor must **pause** here and fire an `input_request` -- it
    /// **never** performs the credential entry itself, and no credential ever
    /// passes through the model/agent context. Maps to
    /// [`PolicyDecision::PauseForInputRequest`].
    SensitiveTransition = 3,
    /// **Class 4 -- ExternalCommitment.** The action would take an
    /// externally-visible, hard-to-reverse committing step: send a
    /// message/email, submit a form, post, publish, confirm a purchase, pay,
    /// transfer, delete/destroy, or otherwise "commit" something the outside
    /// world will see. Requires a **distinct, scoped confirmation** that
    /// expires after [`SCOPED_CONFIRMATION_TTL_SECS`] and **defaults to
    /// reject on timeout**. Maps to
    /// [`PolicyDecision::RequireScopedConfirmation`]. This is the class the
    /// PRD's adversarial "zero-send" acceptance test (row 16a) pins: a `send`
    /// action must classify here and get a scoped confirmation, **never**
    /// [`PolicyDecision::Allow`].
    ExternalCommitment = 4,
    /// **Class 5 -- SensitiveTarget.** The action targets a sensitive
    /// application/surface (password manager, banking/brokerage, health,
    /// system/security settings, admin console, …) -- the class-5 category
    /// set modeled by [`crate::sensitive_categories`]. Requires **per-access
    /// approval** by default ([`PolicyDecision::RequireSensitiveApproval`]),
    /// unless the user has configured that category to
    /// [`CategorySetting::AlwaysAllow`] (then [`PolicyDecision::Allow`]) or
    /// [`CategorySetting::HardBlock`] (then [`PolicyDecision::Reject`]).
    SensitiveTarget = 5,
}

impl ActionClass {
    /// The PRD ordinal (0..=5) of this class, as a stable `u8`. Equivalent to
    /// `self as u8`, exposed as a named method so log/probe call sites read as
    /// intent ("the PRD class number") rather than a bare cast.
    pub fn ordinal(self) -> u8 {
        self as u8
    }

    /// The PRD's short name for this class (e.g. `"external_commitment"`),
    /// snake_case, for logging/diagnostics. Not a serde wire form (this enum
    /// is an in-process gate input, not a wire type) -- purely for
    /// human-readable trace output and the probe.
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

/// The kind of low-level computer-use operation the VLM proposed. This is the
/// *verb* of a proposed action, deliberately modeled as a closed enum (not a
/// free-text string) so the classifier is a total `match`, not a substring
/// search over model output -- the whole point of P0-7 (enforcement, not a
/// prompt).
///
/// A real executor's action vocabulary (Holo3's, or a future in-daemon VLM's)
/// is richer than this, but every richer action reduces to one of these verbs
/// for *classification* purposes: what matters to the policy gate is not the
/// pixel coordinates but "is this reading, moving, drafting, crossing a
/// credential boundary, committing externally, or entering a sensitive app".
/// The follow-up wiring task maps the executor's concrete action type onto
/// this enum at the interception point (see [`WIRING`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionVerb {
    /// Read-only observation: screenshot, read screen, scroll-to-read, hover.
    Observe,
    /// A pointer click at a target (see [`ClickTarget`] for what the target
    /// is -- a plain navigational control vs. a committing button vs. an
    /// auth/credential control determines the class).
    Click { target: ClickTarget },
    /// Typing text into the currently-focused field. `into` describes the
    /// semantic destination of the keystrokes (a draft body vs. a credential
    /// field), which is what determines the class -- not the text itself
    /// (the policy layer never inspects the typed characters; a credential's
    /// *value* must never reach this gate, only the fact that the target is a
    /// credential field).
    Type { into: TypeTarget },
    /// Keyboard navigation / focus movement that commits nothing (Tab,
    /// arrow keys, opening a menu via keyboard).
    Navigate,
    /// An explicit high-level "commit" verb the executor may surface directly
    /// (some agents emit a semantic `submit`/`send` rather than a raw click
    /// on a specific button) -- always class 4 regardless of target, since
    /// its whole meaning is "commit externally".
    Commit { kind: CommitKind },
}

/// What a [`ActionVerb::Click`] is clicking on, at the granularity the policy
/// gate cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickTarget {
    /// A non-committing navigational control: a tab, a menu item that just
    /// navigates, a plain hyperlink, a disclosure triangle. Class 1.
    Navigation,
    /// A control that commits something externally visible when clicked: a
    /// Send / Submit / Post / Publish / Pay / Confirm-purchase / Delete
    /// button. Class 4. `kind` records which commit it is, for the eventual
    /// confirmation prompt's human-readable context.
    CommitButton { kind: CommitKind },
    /// A control that crosses a credential/auth boundary when clicked: a
    /// "Sign in" button that submits a login, an "Authorize"/"Unlock" button
    /// on an auth dialog, an MFA "Approve" button. Class 3 -- pause and fire
    /// an `input_request`; never click it autonomously.
    AuthControl,
}

/// What a [`ActionVerb::Type`] is typing into, at the granularity the policy
/// gate cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeTarget {
    /// A reversible local draft surface: a message/document body, a
    /// not-yet-submitted form field, a search box being composed. Class 2.
    DraftBody,
    /// A credential/secret/MFA field: a password box, an API-key field, a
    /// one-time-code entry. Class 3 -- the executor must **never** type into
    /// this itself; it pauses and fires an `input_request` (`credential` /
    /// `mfa` kind) so the value is entered out-of-band and never enters the
    /// agent context.
    CredentialField,
}

/// Which kind of external commitment a class-4 action represents -- carried
/// through so the eventual scoped-confirmation prompt can say *what* is about
/// to be committed ("send this email", "confirm this $340 payment") rather
/// than a generic "confirm?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKind {
    /// Send a message/email/DM.
    Send,
    /// Submit a form / post / publish.
    Submit,
    /// Confirm a purchase / pay / transfer money.
    Payment,
    /// A destructive/irreversible operation (delete account, wipe, etc.).
    Destructive,
}

impl CommitKind {
    /// A short human-readable phrase for this commit kind, for the eventual
    /// confirmation prompt's context text.
    pub fn label(self) -> &'static str {
        match self {
            CommitKind::Send => "send",
            CommitKind::Submit => "submit",
            CommitKind::Payment => "payment",
            CommitKind::Destructive => "destructive action",
        }
    }
}

/// A single action the VLM proposed during the "think" step, in the form the
/// policy gate classifies. This is the wrapper's *input*.
///
/// `verb` is the operation; `target_bundle_id` (when known) is the macOS
/// bundle ID of the app the action would land in, used **only** for the
/// class-5 sensitive-target check (a `Click`/`Type` into a password manager
/// is class 5 regardless of whether the click is navigational). The
/// bundle-ID membership test is delegated wholesale to
/// [`SensitiveCategories::classify`] -- this module reuses that class-5 data
/// model rather than duplicating any app list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedAction {
    /// What the action does.
    pub verb: ActionVerb,
    /// The bundle ID of the foreground/target app this action lands in, if
    /// the executor could attribute one. `None` when the executor has no
    /// per-app attribution (the honest common case in this daemon today --
    /// see [`crate::audit_log::AppCategory`]'s single `Desktop` variant),
    /// which simply means the class-5 sensitive-target check cannot fire and
    /// the action is classified on its verb alone.
    pub target_bundle_id: Option<String>,
    /// A short human-readable description of the action, for the eventual
    /// confirmation/approval prompt's context and for trace output. **Never**
    /// carries a credential value (a `Type { into: CredentialField }` action
    /// must not put the typed secret here) -- same hard boundary the rest of
    /// this codebase enforces around `input_request` (see
    /// `control_channel::ServerMessage::InputRequest`'s doc).
    pub description: String,
}

impl ProposedAction {
    /// Convenience constructor for an action with no app attribution.
    pub fn new(verb: ActionVerb, description: impl Into<String>) -> Self {
        ProposedAction {
            verb,
            target_bundle_id: None,
            description: description.into(),
        }
    }

    /// Convenience constructor for an action that lands in a known app.
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

/// Classifies a [`ProposedAction`] into its PRD §9 [`ActionClass`].
///
/// This is the real classifier P0-7 requires: a total `match` over the typed
/// action, never a substring search over model-generated text. The rule set,
/// in the order it is applied:
///
/// 1. **Class 5 dominates when a sensitive target is present.** If the action
///    lands in a bundle ID that [`SensitiveCategories::classify`] places in a
///    sensitive category, it is class 5 -- *unless* the verb is a pure
///    read-only [`ActionVerb::Observe`], which stays class 0 (merely *looking
///    at* a sensitive app's already-visible screen changes nothing and is not
///    a "sensitive access" in the PRD's sense; the sensitive-target gate is
///    about *acting into* the app). This ordering is deliberate: a
///    credential-field type or a commit *inside* a password manager is class
///    5 (the more restrictive gate), not class 3/4 -- the sensitive-target
///    approval is the outer boundary. (When class 5's category is
///    `AlwaysAllow`, [`decide`] still lets class-3/4-shaped sub-actions be
///    re-examined by the executor on the next action; this classifier's job
///    is only to name the single most-restrictive applicable class for *this*
///    action.)
/// 2. **Otherwise the verb determines the class** via a total match:
///    - [`ActionVerb::Observe`] → [`ActionClass::Observe`] (0)
///    - [`ActionVerb::Navigate`] → [`ActionClass::Navigate`] (1)
///    - [`ActionVerb::Click`] → depends on [`ClickTarget`]: `Navigation` → 1,
///      `AuthControl` → 3, `CommitButton` → 4.
///    - [`ActionVerb::Type`] → depends on [`TypeTarget`]: `DraftBody` → 2,
///      `CredentialField` → 3.
///    - [`ActionVerb::Commit`] → [`ActionClass::ExternalCommitment`] (4),
///      unconditionally.
///
/// The classifier is total and side-effect-free: every `ProposedAction`
/// yields exactly one class, and the same input always yields the same class.
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

/// The `input_request` kind the executor should raise when [`decide`] returns
/// [`PolicyDecision::PauseForInputRequest`] for a class-3 action. Mirrors
/// `control_channel::InputRequestKind`'s credential/MFA kinds without
/// depending on that wire type here (the policy layer stays free of transport
/// concerns) -- the wiring point translates this into a real
/// `ServerMessage::input_request(...)` call.
///
/// It is deliberately restricted to the two credential-boundary kinds: a
/// class-3 pause is *always* a "manual, out-of-band credential/MFA entry is
/// needed" pause, never a free-text prompt -- matching PRD §9's "credentials
/// never pass through" and `PROTOCOL.md`'s "Credentials never travel on this
/// channel".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseKind {
    /// A password / API key / secret is needed. → `input_request` kind
    /// `credential`.
    Credential,
    /// A multi-factor code / approval is needed. → `input_request` kind
    /// `mfa`.
    Mfa,
}

/// The wrapper's *output*: what the runtime is allowed to do with a proposed
/// action. Only [`PolicyDecision::Allow`] lets it run unchanged; every other
/// variant is a gate the executor must satisfy (or honor) **before** the
/// "act" step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// The action may execute as proposed, immediately. Classes 0-2 by
    /// default, plus a class-5 action whose category the user set to
    /// [`CategorySetting::AlwaysAllow`].
    Allow,
    /// The action is a class-3 credential/auth boundary crossing: **do not
    /// execute it.** Pause the turn and raise an `input_request` of
    /// [`PauseKind`] so the human supplies the credential/MFA out-of-band.
    /// The action itself (typing the password, clicking "sign in") is never
    /// performed by the agent, and no credential ever enters the agent
    /// context. On expiry this follows the existing input-request
    /// expiry-to-safe-pause path (see
    /// `control_channel`), never a silent auto-proceed.
    PauseForInputRequest { kind: PauseKind },
    /// The action is a class-4 external commitment: require a **distinct,
    /// scoped confirmation** (a fresh, single-use approval bound to *this*
    /// action) that expires after `expires_in_secs`
    /// ([`SCOPED_CONFIRMATION_TTL_SECS`] = 60) and **defaults to reject on
    /// timeout**. The action executes **only** if that specific confirmation
    /// is granted before it expires. `commit` records what is being committed
    /// so the confirmation prompt can be specific.
    RequireScopedConfirmation {
        commit: CommitKind,
        expires_in_secs: u64,
    },
    /// The action targets a class-5 sensitive app and the category is set to
    /// its default [`CategorySetting::AlwaysAsk`]: require **per-access
    /// approval** (a sensitive-access consent round trip) before the action
    /// runs. `category_id` is the [`crate::sensitive_categories::SensitiveCategory::id`]
    /// that matched, for the approval prompt's context.
    RequireSensitiveApproval { category_id: String },
    /// The action is refused outright and must not run. Two sources: a class-5
    /// action whose category the user set to [`CategorySetting::HardBlock`],
    /// or any future explicitly-forbidden action. `reason` is
    /// human-readable, for surfacing to the user / logging.
    Reject { reason: String },
}

impl PolicyDecision {
    /// True iff this decision permits the action to reach the runtime
    /// **without any further gate** -- i.e. exactly [`Self::Allow`]. Every
    /// other decision requires a pause/confirmation/approval or is an outright
    /// reject. The single predicate the wiring point's `if` hinges on (see
    /// [`WIRING`]); centralized here so "does this let the action run" is
    /// defined in one place and can never accidentally treat a
    /// pause/confirm/reject as a go.
    pub fn permits_immediate_execution(&self) -> bool {
        matches!(self, PolicyDecision::Allow)
    }

    /// Short label for logging/diagnostics.
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

/// The core decision function: PRD §9's decision table, as real code.
///
/// Given a proposed action already classified by [`classify`], plus the
/// user's class-5 category configuration (for the class-5 branch only),
/// returns the [`PolicyDecision`] the runtime must honor. This is the single
/// enforcement point P0-7 requires -- a total `match` over [`ActionClass`],
/// with **no default-allow fall-through**: every class is handled explicitly,
/// so adding a future class is a compile error here until its policy is
/// decided, rather than silently defaulting to allow.
///
/// The table (PRD §9):
///
/// | Class | Name                 | Default decision                         |
/// |-------|----------------------|------------------------------------------|
/// | 0     | Observe              | [`Allow`](PolicyDecision::Allow)          |
/// | 1     | Navigate             | [`Allow`](PolicyDecision::Allow)          |
/// | 2     | Draft                | [`Allow`](PolicyDecision::Allow)          |
/// | 3     | SensitiveTransition  | [`PauseForInputRequest`](PolicyDecision::PauseForInputRequest) -- never executes, credential never passes through |
/// | 4     | ExternalCommitment   | [`RequireScopedConfirmation`](PolicyDecision::RequireScopedConfirmation) (60s, default reject on timeout) |
/// | 5     | SensitiveTarget      | [`RequireSensitiveApproval`](PolicyDecision::RequireSensitiveApproval) by default; `AlwaysAllow` → [`Allow`]; `HardBlock` → [`Reject`] |
///
/// The class-4 → [`PolicyDecision::RequireScopedConfirmation`] mapping is the
/// row-16a "adversarial zero-send" invariant: a class-4 action can **never**
/// come back [`PolicyDecision::Allow`] from this function -- there is no
/// branch that does so, by construction.
///
/// ## Exact wiring point (for the follow-up interception row)
///
/// In `control_channel::ProtocolHandler::accept`'s read loop, immediately
/// before `self.bridge.handle_message(control_message).await` (the "act"
/// dispatch into `holo serve`), once this daemon has a per-action stream:
///
/// ```ignore
/// let class = policy::classify(&proposed, &self.categories);
/// match policy::decide(class, &self.categories, category_id_for(&proposed)) {
///     d if d.permits_immediate_execution() => self.bridge.handle_message(control_message).await,
///     policy::PolicyDecision::PauseForInputRequest { kind } => {
///         // raise ServerMessage::input_request(.., kind.into(), ..); do NOT dispatch the action
///     }
///     policy::PolicyDecision::RequireScopedConfirmation { commit, expires_in_secs } => {
///         // mint a distinct 60s ApprovalToken (see limits::ApprovalToken), await consent,
///         // dispatch ONLY if granted before expiry; default reject on timeout
///     }
///     policy::PolicyDecision::RequireSensitiveApproval { category_id } => {
///         // sensitive-access consent round trip; dispatch only on approval
///     }
///     policy::PolicyDecision::Reject { reason } => {
///         // ServerMessage::error(reason); do NOT dispatch
///     }
/// }
/// ```
///
/// See [`WIRING`] for the same in prose. It is a hard code gate -- the action
/// is dispatched to the runtime *only* on the `permits_immediate_execution`
/// arm (or, for the gated arms, only after the corresponding
/// pause/confirm/approve is genuinely satisfied), never as a prompt
/// instruction.
///
/// `category_id` is `Some` only for a class-5 action (it names the matched
/// sensitive category); it is looked up by the caller via
/// [`SensitiveCategories::classify`] and passed in so this function does not
/// need the bundle ID again. For classes 0-4 it is ignored (and conventionally
/// `None`).
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

/// One-call convenience: classify `action` and decide in a single step,
/// threading the concrete class-5 category id and, for a class-4 action, the
/// real [`CommitKind`] from the [`ProposedAction`] (which the ordinal-only
/// [`decide`] cannot recover) into the returned decision. This is the form a
/// wiring point holding a full `ProposedAction` should call.
///
/// The class-4 invariant is preserved: this only ever *refines* the
/// `CommitKind` inside a [`PolicyDecision::RequireScopedConfirmation`]; it
/// never turns a class-4 action into an [`PolicyDecision::Allow`].
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

/// Extracts the [`CommitKind`] a verb commits, if any -- used by
/// [`decide_for`] to put the real commit kind into a class-4 decision.
fn commit_kind_of(verb: &ActionVerb) -> Option<CommitKind> {
    match verb {
        ActionVerb::Commit { kind } => Some(*kind),
        ActionVerb::Click {
            target: ClickTarget::CommitButton { kind },
        } => Some(*kind),
        _ => None,
    }
}

/// Human-readable description of the exact wiring point for the follow-up
/// interception row, so it is discoverable both in rustdoc and as a real
/// (probe-printable) string constant rather than only in a comment.
///
/// The policy wrapper is wired by calling [`decide_for`] (or
/// [`classify`]+[`decide`]) in `control_channel::ProtocolHandler::accept`'s
/// read loop, immediately before `self.bridge.handle_message(...)` -- the
/// "act" dispatch -- once this daemon receives a per-action stream to gate
/// (today it forwards whole prompts to `holo serve`, which runs the
/// per-action think/act loop server-side, so the individual actions are not
/// visible here yet). The dispatch happens **only** on
/// [`PolicyDecision::permits_immediate_execution`]; the gated variants
/// (pause / scoped-confirmation / sensitive-approval / reject) each interpose
/// their round trip before -- or instead of -- dispatch. Enforcement lives in
/// this code gate, never in the model's prompt (PRD P0-7).
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
