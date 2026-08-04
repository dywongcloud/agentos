use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holoiroh_wire::ActionId;
use sha2::{Digest, Sha256};

use crate::action_executor::{
    ActionReceipt, DaemonActionExecutor, DesktopAction, ExecutionOutcome,
};
use crate::approval::ApprovalStore;
use crate::semantic_ax::{AxAdapterError, AxPrimitiveAdapter, SemanticAxSource};

const MAX_BINDING_BYTES: usize = 128;
const MAX_UNTRUSTED_RECEIPT_BYTES: usize = 16 * 1024;
const MAX_UNTRUSTED_ERROR_BYTES: usize = 4 * 1024;
static NEXT_ACTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy)]
pub struct AgentLoopLimits {
    pub max_steps: usize,
    pub max_observation_bytes: usize,
    pub max_elapsed: Duration,
}

impl Default for AgentLoopLimits {
    fn default() -> Self {
        Self {
            max_steps: 32,
            max_observation_bytes: 64 * 1024,
            max_elapsed: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedTaskBindings {
    pub goal_id: String,
    pub intent_digest: String,
    pub session_id: String,
    pub run_id: String,
    pub task_id: String,
    pub instruction: String,
}

impl TrustedTaskBindings {
    pub fn new(
        goal_id: &str,
        instruction: &str,
        session_id: &str,
        run_id: &str,
        task_id: &str,
    ) -> Result<Self, TrustedBindingError> {
        validate_binding(goal_id)?;
        validate_binding(session_id)?;
        validate_binding(run_id)?;
        validate_binding(task_id)?;
        if instruction.trim().is_empty() || instruction.len() > 16 * 1024 {
            return Err(TrustedBindingError);
        }
        Ok(Self {
            goal_id: goal_id.to_owned(),
            intent_digest: digest(instruction),
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            task_id: task_id.to_owned(),
            instruction: instruction.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedBindingError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrustedPlannerContext {
    pub observation_json: String,
    pub prior_receipt_json: Option<String>,
    pub prior_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerTurnRequest {
    pub trusted: TrustedTaskBindings,
    pub untrusted: UntrustedPlannerContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAction {
    pub element_id: String,
    pub action: DesktopAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanTurn {
    Act(ModelAction),
    Complete,
}

pub trait BoundedPlanner {
    type Error;

    fn plan_next<'a>(
        &'a mut self,
        request: &'a PlannerTurnRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PlanTurn, Self::Error>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLoopOutcome {
    Completed {
        steps: usize,
    },
    ApprovalRequired {
        steps: usize,
        receipt: ActionReceipt,
    },
    Rejected {
        steps: usize,
        receipt: ActionReceipt,
    },
    StepLimit,
    Deadline,
    Canceled { steps: usize },
}

#[derive(Debug)]
pub enum AgentLoopError<PlannerError, SourceError> {
    Planner(PlannerError),
    Adapter(AxAdapterError<SourceError>),
    InvalidTrustedBindings,
    ObservationTooLarge,
    PlannerContextTooLarge,
    ActionIdExhausted,
    ExecutorPoisoned,
}

struct ActiveRun {
    trusted: TrustedTaskBindings,
    started: Instant,
    next_step: usize,
    prior_receipt_json: Option<String>,
}

pub struct ObservePlanExecuteLoop<S, P> {
    executor: Arc<Mutex<DaemonActionExecutor>>,
    adapter: AxPrimitiveAdapter<S>,
    planner: P,
    limits: AgentLoopLimits,
    active: Option<ActiveRun>,
    canceled: Arc<AtomicBool>,
    execution_gate: Arc<Mutex<()>>,
}

impl<S, P> ObservePlanExecuteLoop<S, P> {
    pub fn new(
        executor: Arc<Mutex<DaemonActionExecutor>>,
        adapter: AxPrimitiveAdapter<S>,
        planner: P,
        limits: AgentLoopLimits,
    ) -> Self {
        Self {
            executor,
            adapter,
            planner,
            limits: AgentLoopLimits {
                max_steps: limits.max_steps.clamp(1, 256),
                max_observation_bytes: limits.max_observation_bytes.clamp(1, 256 * 1024),
                max_elapsed: limits.max_elapsed.min(Duration::from_secs(600)),
            },
            active: None,
            canceled: Arc::new(AtomicBool::new(false)),
            execution_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn executor(&self) -> Arc<Mutex<DaemonActionExecutor>> {
        self.executor.clone()
    }

    pub fn adapter_mut(&mut self) -> &mut AxPrimitiveAdapter<S> {
        &mut self.adapter
    }

    pub fn remaining(&self) -> Duration {
        self.active
            .as_ref()
            .and_then(|active| self.limits.max_elapsed.checked_sub(active.started.elapsed()))
            .unwrap_or_default()
    }

    pub fn execution_gate(&self) -> Arc<Mutex<()>> {
        self.execution_gate.clone()
    }

    pub fn cancellation_handle(&self) -> Arc<AtomicBool> {
        self.canceled.clone()
    }

    pub fn cancel(&mut self) {
        self.canceled.store(true, Ordering::Release);
        self.active = None;
    }
}

impl<S: SemanticAxSource, P: BoundedPlanner> ObservePlanExecuteLoop<S, P> {
    pub async fn run(
        &mut self,
        trusted_goal: &str,
        run_id: &str,
        task_id: &str,
    ) -> Result<AgentLoopOutcome, AgentLoopError<P::Error, S::Error>> {
        let goal_id = format!("goal-{}", digest(trusted_goal));
        self.run_bound(
            TrustedTaskBindings::new(&goal_id, trusted_goal, run_id, run_id, task_id)
                .map_err(|_| AgentLoopError::InvalidTrustedBindings)?,
        )
        .await
    }

    pub async fn run_bound(
        &mut self,
        trusted: TrustedTaskBindings,
    ) -> Result<AgentLoopOutcome, AgentLoopError<P::Error, S::Error>> {
        validate_trusted(&trusted).map_err(|_| AgentLoopError::InvalidTrustedBindings)?;
        self.canceled.store(false, Ordering::Release);
        self.active = Some(ActiveRun {
            trusted,
            started: Instant::now(),
            next_step: 0,
            prior_receipt_json: None,
        });
        self.drive().await
    }

    pub async fn resume_approved(
        &mut self,
        approval_id: &str,
    ) -> Result<AgentLoopOutcome, AgentLoopError<P::Error, S::Error>> {
        let Some(active) = &self.active else {
            return Err(AgentLoopError::InvalidTrustedBindings);
        };
        if active.started.elapsed() >= self.limits.max_elapsed {
            self.executor
                .lock()
                .map_err(|_| AgentLoopError::ExecutorPoisoned)?
                .cancel_approval(approval_id);
            self.active = None;
            return Ok(AgentLoopOutcome::Deadline);
        }
        let steps = active.next_step;
        if self.canceled.load(Ordering::Acquire) {
            self.active = None;
            return Ok(AgentLoopOutcome::Canceled { steps });
        }
        let receipt = {
            let _execution = self
                .execution_gate
                .lock()
                .map_err(|_| AgentLoopError::ExecutorPoisoned)?;
            if self.canceled.load(Ordering::Acquire) {
                self.active = None;
                return Ok(AgentLoopOutcome::Canceled { steps });
            }
            let mut executor = self
                .executor
                .lock()
                .map_err(|_| AgentLoopError::ExecutorPoisoned)?;
            if self.canceled.load(Ordering::Acquire) {
                self.active = None;
                return Ok(AgentLoopOutcome::Canceled { steps });
            }
            executor
                .execute_routed(approval_id, epoch_millis(), &mut self.adapter)
                .map_err(AgentLoopError::Adapter)?
        };
        if receipt.outcome != ExecutionOutcome::Executed {
            self.active = None;
            return Ok(AgentLoopOutcome::Rejected { steps, receipt });
        }
        let serialized = serialize_receipt(&receipt)
            .map_err(|_| AgentLoopError::PlannerContextTooLarge)?;
        self.active
            .as_mut()
            .ok_or(AgentLoopError::InvalidTrustedBindings)?
            .prior_receipt_json = Some(serialized);
        self.drive().await
    }

    async fn drive(
        &mut self,
    ) -> Result<AgentLoopOutcome, AgentLoopError<P::Error, S::Error>> {
        loop {
            let (trusted, started, step, prior_receipt_json) = {
                let active = self
                    .active
                    .as_mut()
                    .ok_or(AgentLoopError::InvalidTrustedBindings)?;
                (
                    active.trusted.clone(),
                    active.started,
                    active.next_step,
                    active.prior_receipt_json.take(),
                )
            };
            if self.canceled.load(Ordering::Acquire) {
                self.active = None;
                return Ok(AgentLoopOutcome::Canceled { steps: step });
            }
            if step >= self.limits.max_steps {
                self.active = None;
                return Ok(AgentLoopOutcome::StepLimit);
            }
            if started.elapsed() >= self.limits.max_elapsed {
                self.active = None;
                return Ok(AgentLoopOutcome::Deadline);
            }
            let observation_json = self
                .adapter
                .observation_json()
                .map_err(AgentLoopError::Adapter)?;
            if observation_json.len() > self.limits.max_observation_bytes {
                self.active = None;
                return Err(AgentLoopError::ObservationTooLarge);
            }
            let request = PlannerTurnRequest {
                trusted: trusted.clone(),
                untrusted: UntrustedPlannerContext {
                    observation_json,
                    prior_receipt_json,
                    prior_error: None,
                },
            };
            validate_context(&request.untrusted, self.limits.max_observation_bytes)
                .map_err(|_| AgentLoopError::PlannerContextTooLarge)?;
            let remaining = self
                .limits
                .max_elapsed
                .checked_sub(started.elapsed())
                .unwrap_or_default();
            if remaining.is_zero() {
                self.active = None;
                return Ok(AgentLoopOutcome::Deadline);
            }
            let turn = match tokio::time::timeout(remaining, self.planner.plan_next(&request)).await
            {
                Ok(result) => result.map_err(AgentLoopError::Planner)?,
                Err(_) => {
                    self.active = None;
                    return Ok(AgentLoopOutcome::Deadline);
                }
            };
            let PlanTurn::Act(model_action) = turn else {
                self.active = None;
                return Ok(AgentLoopOutcome::Completed { steps: step });
            };
            validate_trusted(&trusted).map_err(|_| AgentLoopError::InvalidTrustedBindings)?;
            let action_id = next_action_id().ok_or(AgentLoopError::ActionIdExhausted)?;
            let proposal = self
                .adapter
                .resolve_proposal(
                    trusted.goal_id,
                    trusted.intent_digest,
                    trusted.run_id,
                    trusted.task_id,
                    action_id,
                    &model_action.element_id,
                    model_action.action,
                )
                .map_err(AgentLoopError::Adapter)?;
            if self.canceled.load(Ordering::Acquire) {
                self.active = None;
                return Ok(AgentLoopOutcome::Canceled { steps: step });
            }
            let receipt = {
                let _execution = self
                    .execution_gate
                    .lock()
                    .map_err(|_| AgentLoopError::ExecutorPoisoned)?;
                if self.canceled.load(Ordering::Acquire) {
                    self.active = None;
                    return Ok(AgentLoopOutcome::Canceled { steps: step });
                }
                let mut executor = self
                    .executor
                    .lock()
                    .map_err(|_| AgentLoopError::ExecutorPoisoned)?;
                if self.canceled.load(Ordering::Acquire) {
                    self.active = None;
                    return Ok(AgentLoopOutcome::Canceled { steps: step });
                }
                executor
                    .execute_proposal(
                        &trusted.session_id,
                        &proposal,
                        epoch_millis(),
                        &mut self.adapter,
                    )
                    .map_err(AgentLoopError::Adapter)?
            };
            self.active
                .as_mut()
                .ok_or(AgentLoopError::InvalidTrustedBindings)?
                .next_step = step + 1;
            match receipt.outcome {
                ExecutionOutcome::Executed => {
                    self.active
                        .as_mut()
                        .ok_or(AgentLoopError::InvalidTrustedBindings)?
                        .prior_receipt_json = Some(
                        serialize_receipt(&receipt)
                            .map_err(|_| AgentLoopError::PlannerContextTooLarge)?,
                    );
                }
                ExecutionOutcome::ApprovalRequired(_) => {
                    return Ok(AgentLoopOutcome::ApprovalRequired {
                        steps: step + 1,
                        receipt,
                    });
                }
                _ => {
                    self.active = None;
                    return Ok(AgentLoopOutcome::Rejected {
                        steps: step + 1,
                        receipt,
                    });
                }
            }
        }
    }
}

pub fn shared_daemon_executor(
    approvals: Arc<Mutex<ApprovalStore>>,
    capacity: usize,
) -> Arc<Mutex<DaemonActionExecutor>> {
    Arc::new(Mutex::new(DaemonActionExecutor::new(approvals, capacity)))
}

pub fn build_agent_loop<S, P>(
    executor: Arc<Mutex<DaemonActionExecutor>>,
    source: S,
    planner: P,
    limits: AgentLoopLimits,
) -> ObservePlanExecuteLoop<S, P> {
    ObservePlanExecuteLoop::new(executor, AxPrimitiveAdapter::new(source), planner, limits)
}

fn next_action_id() -> Option<ActionId> {
    NEXT_ACTION_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()
        .map(|value| ActionId(format!("action-{value:016x}")))
}

fn validate_binding(value: &str) -> Result<(), TrustedBindingError> {
    if value.is_empty()
        || value.len() > MAX_BINDING_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(TrustedBindingError);
    }
    Ok(())
}

fn validate_trusted(value: &TrustedTaskBindings) -> Result<(), TrustedBindingError> {
    validate_binding(&value.goal_id)?;
    validate_binding(&value.session_id)?;
    validate_binding(&value.run_id)?;
    validate_binding(&value.task_id)?;
    if value.instruction.trim().is_empty()
        || value.instruction.len() > 16 * 1024
        || value.intent_digest != digest(&value.instruction)
    {
        return Err(TrustedBindingError);
    }
    Ok(())
}

fn validate_context(value: &UntrustedPlannerContext, observation_limit: usize) -> Result<(), ()> {
    if value.observation_json.len() > observation_limit
        || value
            .prior_receipt_json
            .as_ref()
            .is_some_and(|text| text.len() > MAX_UNTRUSTED_RECEIPT_BYTES)
        || value
            .prior_error
            .as_ref()
            .is_some_and(|text| text.len() > MAX_UNTRUSTED_ERROR_BYTES)
    {
        return Err(());
    }
    Ok(())
}

fn serialize_receipt(receipt: &ActionReceipt) -> Result<String, ()> {
    let serialized = serde_json::to_string(&format!("{receipt:?}")).map_err(|_| ())?;
    if serialized.len() > MAX_UNTRUSTED_RECEIPT_BYTES {
        return Err(());
    }
    Ok(serialized)
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"holoiroh/agent-loop-goal/1\0");
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
    data_encoding::HEXLOWER.encode(&hasher.finalize())
}
