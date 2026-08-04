use std::collections::{HashMap, HashSet, VecDeque};

use holoiroh_wire::{
    ActionApprovalRequest, ActionApprovalResponse, ActionId, ApprovalDecision, ApprovalEffect,
    ApprovalRisk,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const DEFAULT_APPROVAL_CAPACITY: usize = 128;
pub const MAX_APPROVAL_TTL_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalError {
    Capacity,
    Unknown,
    Replay,
    Expired,
    BindingMismatch,
    SessionMismatch,
    TaskMismatch,
    StaleBeforeState,
    ProposalSerialization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    Canceled,
}

struct PendingApproval {
    action_id: ActionId,
    proposal_digest: String,
    run_id: String,
    task_id: String,
    before_state_digest: String,
    expires_at: u64,
    session_id: String,
    decision: Option<ApprovalDecision>,
}

pub struct ApprovalStore {
    pending: HashMap<String, PendingApproval>,
    completed: HashSet<String>,
    completed_order: VecDeque<String>,
    capacity: usize,
}

impl ApprovalStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            pending: HashMap::new(),
            completed: HashSet::new(),
            completed_order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &mut self,
        session_id: impl Into<String>,
        action_id: ActionId,
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        effect: ApprovalEffect,
        before_state_digest: impl Into<String>,
        requested_expires_at: u64,
        now: u64,
    ) -> Result<ActionApprovalRequest, ApprovalError> {
        self.prune_expired(now);
        if self.pending.len() >= self.capacity {
            return Err(ApprovalError::Capacity);
        }
        let approval_id = uuid::Uuid::new_v4().to_string();
        let session_id = session_id.into();
        let run_id = run_id.into();
        let task_id = task_id.into();
        let before_state_digest = before_state_digest.into();
        let expires_at = requested_expires_at.min(now.saturating_add(MAX_APPROVAL_TTL_MS));
        let proposal_digest =
            proposal_digest(&action_id, &run_id, &task_id, &effect, &before_state_digest)?;
        let request = ActionApprovalRequest {
            approval_id: approval_id.clone(),
            action_id,
            proposal_digest,
            run_id,
            task_id,
            risk: ApprovalRisk::Critical,
            effect,
            before_state_digest,
            expires_at,
        };
        self.pending.insert(
            approval_id,
            PendingApproval {
                action_id: request.action_id.clone(),
                proposal_digest: request.proposal_digest.clone(),
                run_id: request.run_id.clone(),
                task_id: request.task_id.clone(),
                before_state_digest: request.before_state_digest.clone(),
                expires_at: request.expires_at,
                session_id,
                decision: None,
            },
        );
        Ok(request)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn issue_bound(
        &mut self,
        session_id: impl Into<String>,
        action_id: ActionId,
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        effect: ApprovalEffect,
        before_state_digest: impl Into<String>,
        proposal_digest: String,
        requested_expires_at: u64,
        now: u64,
    ) -> Result<ActionApprovalRequest, ApprovalError> {
        let mut request = self.issue(
            session_id,
            action_id,
            run_id,
            task_id,
            effect,
            before_state_digest,
            requested_expires_at,
            now,
        )?;
        let pending = self
            .pending
            .get_mut(&request.approval_id)
            .ok_or(ApprovalError::Unknown)?;
        pending.proposal_digest.clone_from(&proposal_digest);
        request.proposal_digest = proposal_digest;
        Ok(request)
    }

    pub fn route_response(
        &mut self,
        response: &ActionApprovalResponse,
        envelope_session_id: &str,
        envelope_task_id: Option<&str>,
        now: u64,
    ) -> Result<(), ApprovalError> {
        if self.completed.contains(&response.approval_id) {
            return Err(ApprovalError::Replay);
        }
        let pending = self
            .pending
            .get_mut(&response.approval_id)
            .ok_or(ApprovalError::Unknown)?;
        if now > pending.expires_at {
            let id = response.approval_id.clone();
            self.pending.remove(&id);
            self.remember_completed(id);
            return Err(ApprovalError::Expired);
        }
        if pending.decision.is_some() {
            return Err(ApprovalError::Replay);
        }
        if envelope_session_id != pending.session_id {
            return Err(ApprovalError::SessionMismatch);
        }
        if envelope_task_id != Some(pending.task_id.as_str()) {
            return Err(ApprovalError::TaskMismatch);
        }
        if response.action_id != pending.action_id
            || response.proposal_digest != pending.proposal_digest
        {
            return Err(ApprovalError::BindingMismatch);
        }
        pending.decision = Some(response.decision);
        Ok(())
    }

    pub fn consume(
        &mut self,
        approval_id: &str,
        current_before_state_digest: &str,
        now: u64,
    ) -> Result<ApprovalOutcome, ApprovalError> {
        if self.completed.contains(approval_id) {
            return Err(ApprovalError::Replay);
        }
        let pending = self
            .pending
            .get(approval_id)
            .ok_or(ApprovalError::Unknown)?;
        if now > pending.expires_at {
            let id = approval_id.to_string();
            self.pending.remove(&id);
            self.remember_completed(id);
            return Err(ApprovalError::Expired);
        }
        let decision = pending.decision.ok_or(ApprovalError::Unknown)?;
        if current_before_state_digest != pending.before_state_digest {
            let id = approval_id.to_string();
            self.pending.remove(&id);
            self.remember_completed(id);
            return Err(ApprovalError::StaleBeforeState);
        }
        let id = approval_id.to_string();
        self.pending.remove(&id);
        self.remember_completed(id);
        Ok(match decision {
            ApprovalDecision::Approve => ApprovalOutcome::Approved,
            ApprovalDecision::Deny => ApprovalOutcome::Denied,
            ApprovalDecision::Cancel => ApprovalOutcome::Canceled,
        })
    }

    pub fn resolve(
        &mut self,
        response: &ActionApprovalResponse,
        envelope_session_id: &str,
        envelope_task_id: Option<&str>,
        current_before_state_digest: &str,
        now: u64,
    ) -> Result<ApprovalOutcome, ApprovalError> {
        self.route_response(response, envelope_session_id, envelope_task_id, now)?;
        self.consume(&response.approval_id, current_before_state_digest, now)
    }

    pub fn cancel_approval(&mut self, approval_id: &str) -> bool {
        if self.pending.remove(approval_id).is_some() {
            self.remember_completed(approval_id.to_owned());
            true
        } else {
            false
        }
    }

    pub fn cancel_session(&mut self, session_id: &str) -> usize {
        self.cancel_where(|pending| pending.session_id == session_id)
    }

    pub fn cancel_task_id(&mut self, task_id: &str) -> usize {
        self.cancel_where(|pending| pending.task_id == task_id)
    }

    pub fn cancel_task(&mut self, run_id: &str, task_id: &str) -> usize {
        self.cancel_where(|pending| pending.run_id == run_id && pending.task_id == task_id)
    }

    fn cancel_where(&mut self, matches: impl Fn(&PendingApproval) -> bool) -> usize {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| matches(pending))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.pending.remove(id);
            self.remember_completed(id.clone());
        }
        ids.len()
    }

    pub fn prune_expired(&mut self, now: u64) -> usize {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, pending)| now > pending.expires_at)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.pending.remove(id);
            self.remember_completed(id.clone());
        }
        ids.len()
    }

    fn remember_completed(&mut self, id: String) {
        if self.completed.insert(id.clone()) {
            self.completed_order.push_back(id);
        }
        while self.completed_order.len() > self.capacity {
            if let Some(oldest) = self.completed_order.pop_front() {
                self.completed.remove(&oldest);
            }
        }
    }
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self::new(DEFAULT_APPROVAL_CAPACITY)
    }
}

pub fn response_for(
    request: &ActionApprovalRequest,
    decision: ApprovalDecision,
) -> ActionApprovalResponse {
    ActionApprovalResponse {
        approval_id: request.approval_id.clone(),
        action_id: request.action_id.clone(),
        proposal_digest: request.proposal_digest.clone(),
        decision,
    }
}

#[derive(Serialize)]
struct CanonicalProposal<'a> {
    action_id: &'a ActionId,
    run_id: &'a str,
    task_id: &'a str,
    effect: &'a ApprovalEffect,
    before_state_digest: &'a str,
}

fn proposal_digest(
    action_id: &ActionId,
    run_id: &str,
    task_id: &str,
    effect: &ApprovalEffect,
    before_state_digest: &str,
) -> Result<String, ApprovalError> {
    let proposal = CanonicalProposal {
        action_id,
        run_id,
        task_id,
        effect,
        before_state_digest,
    };
    let bytes = serde_json::to_vec(&proposal).map_err(|_| ApprovalError::ProposalSerialization)?;
    let mut hasher = Sha256::new();
    hasher.update(b"holoiroh/action-approval/1\0");
    hasher.update(bytes);
    Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
}
