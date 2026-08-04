use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use holoiroh_wire::{ActionApprovalRequest, ActionApprovalResponse, ActionId, ApprovalEffect};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::approval::{ApprovalError, ApprovalOutcome, ApprovalStore};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedAction {
    pub action_id: ActionId,
    pub run_id: String,
    pub task_id: String,
    pub effect: ApprovalEffect,
    pub before_state_digest: String,
}

pub trait BeforeStateSource {
    type Error;

    fn digest(&mut self, action: &TypedAction) -> Result<String, Self::Error>;
}

pub trait ActionBackend {
    type Error;

    fn execute(&mut self, action: &TypedAction) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionExecutionError<StateError, BackendError> {
    Approval(ApprovalError),
    UnknownProposal,
    ProposalBindingMismatch,
    NotApproved(ApprovalOutcome),
    DuplicateAction,
    State(StateError),
    StateChanged,
    Backend(BackendError),
}

pub struct TypedActionExecutor {
    approvals: Arc<Mutex<ApprovalStore>>,
    proposals: HashMap<String, TypedAction>,
    completed: HashSet<ActionId>,
    completed_order: VecDeque<ActionId>,
    capacity: usize,
}

impl TypedActionExecutor {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            approvals: Arc::new(Mutex::new(ApprovalStore::new(capacity))),
            proposals: HashMap::new(),
            completed: HashSet::new(),
            completed_order: VecDeque::new(),
            capacity,
        }
    }

    pub fn with_approval_store(capacity: usize, approvals: Arc<Mutex<ApprovalStore>>) -> Self {
        let capacity = capacity.max(1);
        Self {
            approvals,
            proposals: HashMap::new(),
            completed: HashSet::new(),
            completed_order: VecDeque::new(),
            capacity,
        }
    }

    pub fn approval_store(&self) -> Arc<Mutex<ApprovalStore>> {
        self.approvals.clone()
    }

    pub fn propose(
        &mut self,
        session_id: impl Into<String>,
        action: TypedAction,
        requested_expires_at: u64,
        now: u64,
    ) -> Result<ActionApprovalRequest, ApprovalError> {
        if self.completed.contains(&action.action_id)
            || self
                .proposals
                .values()
                .any(|pending| pending.action_id == action.action_id)
        {
            return Err(ApprovalError::Replay);
        }
        let request = self
            .approvals
            .lock()
            .expect("approval store lock poisoned")
            .issue(
                session_id,
                action.action_id.clone(),
                action.run_id.clone(),
                action.task_id.clone(),
                action.effect.clone(),
                action.before_state_digest.clone(),
                requested_expires_at,
                now,
            )?;
        self.proposals.insert(request.approval_id.clone(), action);
        Ok(request)
    }

    pub fn execute<State, Backend>(
        &mut self,
        response: &ActionApprovalResponse,
        envelope_session_id: &str,
        envelope_task_id: Option<&str>,
        now: u64,
        state: &mut State,
        backend: &mut Backend,
    ) -> Result<(), ActionExecutionError<State::Error, Backend::Error>>
    where
        State: BeforeStateSource,
        Backend: ActionBackend,
    {
        let action = self
            .proposals
            .get(&response.approval_id)
            .cloned()
            .ok_or(ActionExecutionError::UnknownProposal)?;
        if response.action_id != action.action_id
            || envelope_task_id != Some(action.task_id.as_str())
        {
            return Err(ActionExecutionError::ProposalBindingMismatch);
        }
        if self.completed.contains(&action.action_id) {
            return Err(ActionExecutionError::DuplicateAction);
        }

        self.approvals
            .lock()
            .expect("approval store lock poisoned")
            .route_response(response, envelope_session_id, envelope_task_id, now)
            .map_err(ActionExecutionError::Approval)?;
        let observed = state.digest(&action).map_err(ActionExecutionError::State)?;
        if observed != action.before_state_digest {
            return Err(ActionExecutionError::StateChanged);
        }
        let immediate = state.digest(&action).map_err(ActionExecutionError::State)?;
        if immediate != action.before_state_digest {
            return Err(ActionExecutionError::StateChanged);
        }
        let outcome = self
            .approvals
            .lock()
            .expect("approval store lock poisoned")
            .consume(&response.approval_id, &immediate, now)
            .map_err(ActionExecutionError::Approval)?;
        self.proposals.remove(&response.approval_id);
        if outcome != ApprovalOutcome::Approved {
            return Err(ActionExecutionError::NotApproved(outcome));
        }

        self.remember_completed(action.action_id.clone());
        backend
            .execute(&action)
            .map_err(ActionExecutionError::Backend)
    }

    pub fn cancel_session(&mut self, session_id: &str) -> usize {
        let canceled = self
            .approvals
            .lock()
            .expect("approval store lock poisoned")
            .cancel_session(session_id);
        if canceled > 0 {
            self.proposals.clear();
        }
        canceled
    }

    fn remember_completed(&mut self, action_id: ActionId) {
        if self.completed.insert(action_id.clone()) {
            self.completed_order.push_back(action_id);
        }
        while self.completed_order.len() > self.capacity {
            if let Some(oldest) = self.completed_order.pop_front() {
                self.completed.remove(&oldest);
            }
        }
    }
}

impl Default for TypedActionExecutor {
    fn default() -> Self {
        Self::new(crate::approval::DEFAULT_APPROVAL_CAPACITY)
    }
}

pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_DRAFT_BYTES: usize = 16_384;
pub const MAX_SCROLL_POINTS: i32 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    ReadOnly,
    ReversibleLocal,
    CredentialBoundary,
    ExternalCommitment,
    SensitiveTarget,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommitAction {
    SendMessage,
    SubmitForm,
    Publish,
    Purchase,
    TransferFunds,
    DeleteItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationAction {
    SemanticActivate,
    CoordinateActivate { x: i32, y: i32 },
    Scroll { horizontal: i32, vertical: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopAction {
    Observe,
    Navigate(NavigationAction),
    Focus,
    DraftText { text: String },
    Commit(CommitAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationRef {
    pub observation_id: String,
    pub before_state_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRef {
    pub bundle_id: String,
    pub window_id: String,
    pub element_id: String,
    pub expected_role: String,
    pub expected_title_digest: String,
    pub expected_value_digest: Option<String>,
    pub sensitive: bool,
    pub credential: bool,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionProposal {
    pub goal_id: String,
    pub intent_digest: String,
    pub run_id: String,
    pub task_id: String,
    pub action_id: ActionId,
    pub observation: ObservationRef,
    pub target: TargetRef,
    pub action: DesktopAction,
    pub proposal_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Executed,
    ApprovalRequired(ActionApprovalRequest),
    Denied,
    Canceled,
    Expired,
    Stale,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionReceipt {
    pub action_id: ActionId,
    pub proposal_digest: String,
    pub risk: ActionRisk,
    pub before_state_digest: String,
    pub outcome: ExecutionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalError {
    InvalidIdentifier,
    InvalidDigest,
    DraftTooLarge,
    ScrollOutOfBounds,
    UnresolvedTarget,
    CredentialTarget,
    Unsupported,
}

pub fn classify_action(action: &DesktopAction, target: &TargetRef) -> ActionRisk {
    if !target.resolved {
        return ActionRisk::Unsupported;
    }
    if target.credential {
        return ActionRisk::CredentialBoundary;
    }
    if target.sensitive && !matches!(action, DesktopAction::Observe) {
        return ActionRisk::SensitiveTarget;
    }
    match action {
        DesktopAction::Observe => ActionRisk::ReadOnly,
        DesktopAction::Navigate(NavigationAction::Scroll {
            horizontal,
            vertical,
        }) if horizontal.unsigned_abs() <= MAX_SCROLL_POINTS as u32
            && vertical.unsigned_abs() <= MAX_SCROLL_POINTS as u32 =>
        {
            ActionRisk::ReadOnly
        }
        DesktopAction::Navigate(NavigationAction::Scroll { .. }) => ActionRisk::Unsupported,
        DesktopAction::Navigate(_) | DesktopAction::Focus | DesktopAction::DraftText { .. } => {
            ActionRisk::ReversibleLocal
        }
        DesktopAction::Commit(_) => ActionRisk::ExternalCommitment,
    }
}

pub fn canonical_proposal_digest(proposal: &ActionProposal) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holoiroh/desktop-action-proposal/1\0");
    for value in [
        proposal.goal_id.as_str(),
        proposal.intent_digest.as_str(),
        proposal.run_id.as_str(),
        proposal.task_id.as_str(),
        proposal.action_id.0.as_str(),
        proposal.observation.observation_id.as_str(),
        proposal.observation.before_state_digest.as_str(),
        proposal.target.bundle_id.as_str(),
        proposal.target.window_id.as_str(),
        proposal.target.element_id.as_str(),
        proposal.target.expected_role.as_str(),
        proposal.target.expected_title_digest.as_str(),
        proposal
            .target
            .expected_value_digest
            .as_deref()
            .unwrap_or(""),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update([
        u8::from(proposal.target.sensitive),
        u8::from(proposal.target.credential),
        u8::from(proposal.target.resolved),
    ]);
    match &proposal.action {
        DesktopAction::Observe => hasher.update(b"observe"),
        DesktopAction::Navigate(NavigationAction::SemanticActivate) => {
            hasher.update(b"navigate-semantic")
        }
        DesktopAction::Navigate(NavigationAction::CoordinateActivate { x, y }) => {
            hasher.update(b"navigate-coordinate");
            hasher.update(x.to_be_bytes());
            hasher.update(y.to_be_bytes());
        }
        DesktopAction::Navigate(NavigationAction::Scroll {
            horizontal,
            vertical,
        }) => {
            hasher.update(b"scroll");
            hasher.update(horizontal.to_be_bytes());
            hasher.update(vertical.to_be_bytes());
        }
        DesktopAction::Focus => hasher.update(b"focus"),
        DesktopAction::DraftText { text } => {
            hasher.update(b"draft-text-digest");
            hasher.update(Sha256::digest(text.as_bytes()));
        }
        DesktopAction::Commit(commit) => {
            hasher.update(b"commit");
            hasher.update([*commit as u8]);
        }
    }
    data_encoding::HEXLOWER.encode(&hasher.finalize())
}

pub fn validate_proposal(proposal: &ActionProposal) -> Result<ActionRisk, ProposalError> {
    for id in [
        proposal.goal_id.as_str(),
        proposal.run_id.as_str(),
        proposal.task_id.as_str(),
        proposal.action_id.0.as_str(),
        proposal.observation.observation_id.as_str(),
        proposal.target.bundle_id.as_str(),
        proposal.target.window_id.as_str(),
        proposal.target.element_id.as_str(),
    ] {
        if id.is_empty()
            || id.len() > MAX_IDENTIFIER_BYTES
            || !id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(ProposalError::InvalidIdentifier);
        }
    }
    for digest in [
        proposal.proposal_digest.as_str(),
        proposal.intent_digest.as_str(),
        proposal.observation.before_state_digest.as_str(),
        proposal.target.expected_title_digest.as_str(),
    ]
    .into_iter()
    .chain(proposal.target.expected_value_digest.as_deref())
    {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ProposalError::InvalidDigest);
        }
    }
    if !proposal.target.resolved {
        return Err(ProposalError::UnresolvedTarget);
    }
    if proposal.target.credential {
        return Err(ProposalError::CredentialTarget);
    }
    match &proposal.action {
        DesktopAction::DraftText { text } if text.len() > MAX_DRAFT_BYTES => {
            return Err(ProposalError::DraftTooLarge);
        }
        DesktopAction::Navigate(NavigationAction::Scroll {
            horizontal,
            vertical,
        }) if horizontal.unsigned_abs() > MAX_SCROLL_POINTS as u32
            || vertical.unsigned_abs() > MAX_SCROLL_POINTS as u32 =>
        {
            return Err(ProposalError::ScrollOutOfBounds);
        }
        _ => {}
    }
    if canonical_proposal_digest(proposal) != proposal.proposal_digest {
        return Err(ProposalError::InvalidDigest);
    }
    let risk = classify_action(&proposal.action, &proposal.target);
    if risk == ActionRisk::Unsupported {
        Err(ProposalError::Unsupported)
    } else {
        Ok(risk)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshTargetState {
    pub bundle_id: String,
    pub window_id: String,
    pub element_id: String,
    pub role: String,
    pub title_digest: String,
    pub value_digest: Option<String>,
    pub before_state_digest: String,
    pub bounds: Option<(i32, i32, i32, i32)>,
}

impl FreshTargetState {
    pub fn matches(&self, proposal: &ActionProposal) -> bool {
        self.bundle_id == proposal.target.bundle_id
            && self.window_id == proposal.target.window_id
            && self.element_id == proposal.target.element_id
            && self.role == proposal.target.expected_role
            && self.title_digest == proposal.target.expected_title_digest
            && self.value_digest == proposal.target.expected_value_digest
            && self.before_state_digest == proposal.observation.before_state_digest
    }
}

pub trait PrimitiveAdapter {
    type Error;

    fn observe(&mut self, target: &TargetRef) -> Result<FreshTargetState, Self::Error>;
    fn execute_observe(&mut self, target: &TargetRef) -> Result<(), Self::Error>;
    fn execute_navigation(
        &mut self,
        target: &TargetRef,
        action: &NavigationAction,
        fresh_bounds: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), Self::Error>;
    fn execute_focus(&mut self, target: &TargetRef) -> Result<(), Self::Error>;
    fn execute_draft(&mut self, target: &TargetRef, text: &str) -> Result<(), Self::Error>;
    fn execute_commit(
        &mut self,
        target: &TargetRef,
        action: CommitAction,
    ) -> Result<(), Self::Error>;
}

pub fn execute_primitive<A: PrimitiveAdapter>(
    proposal: &ActionProposal,
    adapter: &mut A,
) -> Result<ActionReceipt, A::Error> {
    let risk = match validate_proposal(proposal) {
        Ok(risk) => risk,
        Err(_) => {
            return Ok(ActionReceipt {
                action_id: proposal.action_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                risk: ActionRisk::Unsupported,
                before_state_digest: proposal.observation.before_state_digest.clone(),
                outcome: ExecutionOutcome::Unsupported,
            });
        }
    };
    let fresh = adapter.observe(&proposal.target)?;
    if !fresh.matches(proposal) {
        return Ok(ActionReceipt {
            action_id: proposal.action_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            risk,
            before_state_digest: fresh.before_state_digest,
            outcome: ExecutionOutcome::Stale,
        });
    }
    if matches!(
        risk,
        ActionRisk::ExternalCommitment | ActionRisk::SensitiveTarget
    ) {
        return Ok(ActionReceipt {
            action_id: proposal.action_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
            risk,
            before_state_digest: fresh.before_state_digest,
            outcome: ExecutionOutcome::Unsupported,
        });
    }
    match &proposal.action {
        DesktopAction::Observe => adapter.execute_observe(&proposal.target)?,
        DesktopAction::Navigate(action) => {
            if let NavigationAction::CoordinateActivate { x, y } = action {
                let Some((left, top, width, height)) = fresh.bounds else {
                    return Ok(stale_receipt(proposal, risk, fresh.before_state_digest));
                };
                if *x < left
                    || *y < top
                    || *x >= left.saturating_add(width)
                    || *y >= top.saturating_add(height)
                {
                    return Ok(stale_receipt(proposal, risk, fresh.before_state_digest));
                }
            }
            adapter.execute_navigation(&proposal.target, action, fresh.bounds)?
        }
        DesktopAction::Focus => adapter.execute_focus(&proposal.target)?,
        DesktopAction::DraftText { text } => adapter.execute_draft(&proposal.target, text)?,
        DesktopAction::Commit(_) => unreachable!(),
    }
    Ok(ActionReceipt {
        action_id: proposal.action_id.clone(),
        proposal_digest: proposal.proposal_digest.clone(),
        risk,
        before_state_digest: fresh.before_state_digest,
        outcome: ExecutionOutcome::Executed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionAuditRecord {
    pub goal_id: String,
    pub run_id: String,
    pub task_id: String,
    pub action_id: ActionId,
    pub proposal_digest: String,
    pub risk: ActionRisk,
    pub approval_id: Option<String>,
    pub precondition_matched: bool,
    pub outcome: &'static str,
}

pub trait ActionAuditSink {
    fn append(&mut self, record: ActionAuditRecord);
}

pub struct PersistentActionAudit {
    logger: Arc<crate::audit_log::AuditLogger>,
}

impl PersistentActionAudit {
    pub fn new(logger: Arc<crate::audit_log::AuditLogger>) -> Self {
        Self { logger }
    }
}

impl ActionAuditSink for PersistentActionAudit {
    fn append(&mut self, record: ActionAuditRecord) {
        if let Err(error) = self.logger.append(&record) {
            tracing::warn!(error = %error, "typed action audit append failed");
        }
    }
}

struct PendingDaemonAction {
    session_id: String,
    proposal: ActionProposal,
}

pub struct DaemonActionExecutor {
    approvals: Arc<Mutex<ApprovalStore>>,
    pending: HashMap<String, PendingDaemonAction>,
    completed: HashSet<ActionId>,
    capacity: usize,
    audit: Option<PersistentActionAudit>,
}

impl DaemonActionExecutor {
    pub fn new(approvals: Arc<Mutex<ApprovalStore>>, capacity: usize) -> Self {
        Self {
            approvals,
            pending: HashMap::new(),
            completed: HashSet::new(),
            capacity: capacity.max(1),
            audit: None,
        }
    }

    pub fn with_audit(mut self, logger: Arc<crate::audit_log::AuditLogger>) -> Self {
        self.audit = Some(PersistentActionAudit::new(logger));
        self
    }

    pub fn approval_store(&self) -> Arc<Mutex<ApprovalStore>> {
        self.approvals.clone()
    }

    fn audit(
        &mut self,
        proposal: &ActionProposal,
        risk: ActionRisk,
        approval_id: Option<String>,
        precondition_matched: bool,
        outcome: &'static str,
    ) {
        if let Some(audit) = &mut self.audit {
            audit.append(ActionAuditRecord {
                goal_id: proposal.goal_id.clone(),
                run_id: proposal.run_id.clone(),
                task_id: proposal.task_id.clone(),
                action_id: proposal.action_id.clone(),
                proposal_digest: proposal.proposal_digest.clone(),
                risk,
                approval_id,
                precondition_matched,
                outcome,
            });
        }
    }

    pub fn execute_proposal<A: PrimitiveAdapter>(
        &mut self,
        session_id: &str,
        proposal: &ActionProposal,
        now: u64,
        adapter: &mut A,
    ) -> Result<ActionReceipt, A::Error> {
        if classify_action(&proposal.action, &proposal.target) == ActionRisk::ExternalCommitment {
            let outcome = self.issue_approval(
                session_id,
                proposal.clone(),
                now.saturating_add(crate::approval::MAX_APPROVAL_TTL_MS),
                now,
                adapter,
            )?;
            return Ok(receipt(proposal, ActionRisk::ExternalCommitment, outcome));
        }
        self.execute_immediate(proposal, adapter)
    }

    pub fn execute_immediate<A: PrimitiveAdapter>(
        &mut self,
        proposal: &ActionProposal,
        adapter: &mut A,
    ) -> Result<ActionReceipt, A::Error> {
        let result = execute_primitive(proposal, adapter)?;
        let outcome = match &result.outcome {
            ExecutionOutcome::Executed => "executed",
            ExecutionOutcome::Stale => "stale",
            ExecutionOutcome::Unsupported => "unsupported",
            ExecutionOutcome::Denied => "denied",
            ExecutionOutcome::Canceled => "canceled",
            ExecutionOutcome::Expired => "expired",
            ExecutionOutcome::ApprovalRequired(_) => "approval_required",
        };
        self.audit(
            proposal,
            result.risk,
            None,
            result.outcome == ExecutionOutcome::Executed,
            outcome,
        );
        Ok(result)
    }

    pub fn issue_approval<A: PrimitiveAdapter>(
        &mut self,
        session_id: &str,
        proposal: ActionProposal,
        expires_at: u64,
        now: u64,
        adapter: &mut A,
    ) -> Result<ExecutionOutcome, A::Error> {
        let risk = match validate_proposal(&proposal) {
            Ok(ActionRisk::ExternalCommitment) => ActionRisk::ExternalCommitment,
            Ok(risk) => {
                self.audit(&proposal, risk, None, false, "unsupported");
                return Ok(ExecutionOutcome::Unsupported);
            }
            Err(_) => {
                self.audit(
                    &proposal,
                    ActionRisk::Unsupported,
                    None,
                    false,
                    "unsupported",
                );
                return Ok(ExecutionOutcome::Unsupported);
            }
        };
        if self.pending.len() >= self.capacity || self.completed.contains(&proposal.action_id) {
            self.audit(&proposal, risk, None, false, "unsupported");
            return Ok(ExecutionOutcome::Unsupported);
        }
        let first = match adapter.observe(&proposal.target) {
            Ok(first) => first,
            Err(error) => {
                self.audit(&proposal, risk, None, false, "observation_error");
                return Err(error);
            }
        };
        if !first.matches(&proposal) || classify_action(&proposal.action, &proposal.target) != risk
        {
            self.audit(&proposal, risk, None, false, "stale");
            return Ok(ExecutionOutcome::Stale);
        }
        let effect = ApprovalEffect {
            app: proposal.target.bundle_id.clone(),
            target: proposal.target.element_id.clone(),
            material: match proposal.action {
                DesktopAction::Commit(CommitAction::SendMessage) => "send_message",
                DesktopAction::Commit(CommitAction::SubmitForm) => "submit_form",
                DesktopAction::Commit(CommitAction::Publish) => "publish",
                DesktopAction::Commit(CommitAction::Purchase) => "purchase",
                DesktopAction::Commit(CommitAction::TransferFunds) => "transfer_funds",
                DesktopAction::Commit(CommitAction::DeleteItem) => "delete_item",
                _ => "sensitive_mutation",
            }
            .to_owned(),
        };
        let issued = {
            self.approvals
                .lock()
                .expect("approval store lock poisoned")
                .issue_bound(
                    session_id,
                    proposal.action_id.clone(),
                    proposal.run_id.clone(),
                    proposal.task_id.clone(),
                    effect,
                    first.before_state_digest,
                    proposal.proposal_digest.clone(),
                    expires_at,
                    now,
                )
        };
        let request = match issued {
            Ok(request) => request,
            Err(_) => {
                self.audit(&proposal, risk, None, true, "unsupported");
                return Ok(ExecutionOutcome::Unsupported);
            }
        };
        self.audit(
            &proposal,
            risk,
            Some(request.approval_id.clone()),
            true,
            "approval_required",
        );
        self.pending.insert(
            request.approval_id.clone(),
            PendingDaemonAction {
                session_id: session_id.to_owned(),
                proposal,
            },
        );
        Ok(ExecutionOutcome::ApprovalRequired(request))
    }

    pub fn execute_routed<A: PrimitiveAdapter>(
        &mut self,
        approval_id: &str,
        now: u64,
        adapter: &mut A,
    ) -> Result<ActionReceipt, A::Error> {
        let Some(pending) = self.pending.remove(approval_id) else {
            return Ok(unsupported_receipt(ActionId(String::new()), String::new()));
        };
        let proposal = pending.proposal;
        let risk = classify_action(&proposal.action, &proposal.target);
        let first = match adapter.observe(&proposal.target) {
            Ok(first) => first,
            Err(error) => {
                self.approvals
                    .lock()
                    .expect("approval store lock poisoned")
                    .cancel_approval(approval_id);
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    false,
                    "observation_error",
                );
                return Err(error);
            }
        };
        if !first.matches(&proposal) || classify_action(&proposal.action, &proposal.target) != risk
        {
            self.approvals
                .lock()
                .expect("approval store lock poisoned")
                .cancel_approval(approval_id);
            self.audit(
                &proposal,
                risk,
                Some(approval_id.to_owned()),
                false,
                "stale",
            );
            return Ok(stale_receipt(&proposal, risk, first.before_state_digest));
        }
        let second = match adapter.observe(&proposal.target) {
            Ok(second) => second,
            Err(error) => {
                self.approvals
                    .lock()
                    .expect("approval store lock poisoned")
                    .cancel_approval(approval_id);
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    false,
                    "observation_error",
                );
                return Err(error);
            }
        };
        if !second.matches(&proposal) || second != first {
            self.approvals
                .lock()
                .expect("approval store lock poisoned")
                .cancel_approval(approval_id);
            self.audit(
                &proposal,
                risk,
                Some(approval_id.to_owned()),
                false,
                "stale",
            );
            return Ok(stale_receipt(&proposal, risk, second.before_state_digest));
        }
        let outcome = self
            .approvals
            .lock()
            .expect("approval store lock poisoned")
            .consume(approval_id, &second.before_state_digest, now);
        match outcome {
            Ok(ApprovalOutcome::Approved) => {}
            Ok(ApprovalOutcome::Denied) => {
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    true,
                    "denied",
                );
                return Ok(receipt(&proposal, risk, ExecutionOutcome::Denied));
            }
            Ok(ApprovalOutcome::Canceled) => {
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    true,
                    "canceled",
                );
                return Ok(receipt(&proposal, risk, ExecutionOutcome::Canceled));
            }
            Err(ApprovalError::Expired) => {
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    true,
                    "expired",
                );
                return Ok(receipt(&proposal, risk, ExecutionOutcome::Expired));
            }
            Err(_) => {
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    true,
                    "unsupported",
                );
                return Ok(receipt(&proposal, risk, ExecutionOutcome::Unsupported));
            }
        }
        match &proposal.action {
            DesktopAction::Commit(commit) => {
                if let Err(error) = adapter.execute_commit(&proposal.target, *commit) {
                    self.audit(
                        &proposal,
                        risk,
                        Some(approval_id.to_owned()),
                        true,
                        "adapter_error",
                    );
                    return Err(error);
                }
            }
            _ => {
                self.audit(
                    &proposal,
                    risk,
                    Some(approval_id.to_owned()),
                    true,
                    "unsupported",
                );
                return Ok(receipt(&proposal, risk, ExecutionOutcome::Unsupported));
            }
        }
        self.completed.insert(proposal.action_id.clone());
        self.audit(
            &proposal,
            risk,
            Some(approval_id.to_owned()),
            true,
            "executed",
        );
        Ok(receipt(&proposal, risk, ExecutionOutcome::Executed))
    }

    pub fn cancel_approval(&mut self, approval_id: &str) -> bool {
        let store_canceled = self
            .approvals
            .lock()
            .expect("approval store lock poisoned")
            .cancel_approval(approval_id);
        let Some(pending) = self.pending.remove(approval_id) else {
            return store_canceled;
        };
        let risk = classify_action(&pending.proposal.action, &pending.proposal.target);
        self.audit(
            &pending.proposal,
            risk,
            Some(approval_id.to_owned()),
            true,
            "canceled",
        );
        true
    }

    pub fn cancel_task_id(&mut self, task_id: &str) -> usize {
        self.approvals
            .lock()
            .expect("approval store lock poisoned")
            .cancel_task_id(task_id);
        self.cancel_pending_where(|pending| pending.proposal.task_id == task_id)
    }

    pub fn cancel_session(&mut self, session_id: &str) -> usize {
        self.approvals
            .lock()
            .expect("approval store lock poisoned")
            .cancel_session(session_id);
        self.cancel_pending_where(|pending| pending.session_id == session_id)
    }

    fn cancel_pending_where(
        &mut self,
        predicate: impl Fn(&PendingDaemonAction) -> bool,
    ) -> usize {
        let approval_ids: Vec<String> = self
            .pending
            .iter()
            .filter_map(|(approval_id, pending)| {
                predicate(pending).then(|| approval_id.clone())
            })
            .collect();
        for approval_id in &approval_ids {
            if let Some(pending) = self.pending.remove(approval_id) {
                let risk = classify_action(&pending.proposal.action, &pending.proposal.target);
                self.audit(
                    &pending.proposal,
                    risk,
                    Some(approval_id.clone()),
                    true,
                    "canceled",
                );
            }
        }
        approval_ids.len()
    }
}

fn receipt(
    proposal: &ActionProposal,
    risk: ActionRisk,
    outcome: ExecutionOutcome,
) -> ActionReceipt {
    ActionReceipt {
        action_id: proposal.action_id.clone(),
        proposal_digest: proposal.proposal_digest.clone(),
        risk,
        before_state_digest: proposal.observation.before_state_digest.clone(),
        outcome,
    }
}

fn stale_receipt(proposal: &ActionProposal, risk: ActionRisk, digest: String) -> ActionReceipt {
    ActionReceipt {
        action_id: proposal.action_id.clone(),
        proposal_digest: proposal.proposal_digest.clone(),
        risk,
        before_state_digest: digest,
        outcome: ExecutionOutcome::Stale,
    }
}

fn unsupported_receipt(action_id: ActionId, proposal_digest: String) -> ActionReceipt {
    ActionReceipt {
        action_id,
        proposal_digest,
        risk: ActionRisk::Unsupported,
        before_state_digest: String::new(),
        outcome: ExecutionOutcome::Unsupported,
    }
}
