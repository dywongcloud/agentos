use std::collections::VecDeque;
use std::fs;
use std::sync::{Arc, Mutex};

use holoiroh_daemon::action_executor::{
    ActionBackend, ActionExecutionError, ActionProposal, ActionRisk, BeforeStateSource,
    CommitAction, DaemonActionExecutor, DesktopAction, FreshTargetState, NavigationAction,
    ObservationRef, PrimitiveAdapter, TargetRef, TypedAction, TypedActionExecutor,
    canonical_proposal_digest, classify_action, execute_primitive, validate_proposal,
};
use holoiroh_daemon::approval::{ApprovalError, ApprovalOutcome, ApprovalStore, response_for};
use holoiroh_daemon::audit_log::AuditLogger;
use holoiroh_daemon::control_channel::{
    admit_post_signature_envelope, verify_client_envelope_for_probing,
};
use holoiroh_daemon::execution_mode::ExecutionMode;
use holoiroh_wire::{
    ActionId, ApprovalDecision, ApprovalEffect, ClientMessage, EnvelopeDirection,
    InboundEnvelopeState, TaskEnvelope, encode_ed25519_signature,
};

const NOW: u64 = 1_000_000;

#[derive(Default)]
struct State {
    values: Vec<Result<&'static str, &'static str>>,
}

impl BeforeStateSource for State {
    type Error = &'static str;

    fn digest(&mut self, _action: &TypedAction) -> Result<String, Self::Error> {
        self.values.remove(0).map(str::to_owned)
    }
}

#[derive(Default)]
struct Backend {
    effects: Vec<ActionId>,
    fail: bool,
}

impl ActionBackend for Backend {
    type Error = &'static str;

    fn execute(&mut self, action: &TypedAction) -> Result<(), Self::Error> {
        self.effects.push(action.action_id.clone());
        if self.fail {
            Err("backend failed")
        } else {
            Ok(())
        }
    }
}

fn action(id: &str) -> TypedAction {
    TypedAction {
        action_id: ActionId(id.to_owned()),
        run_id: "run-1".to_owned(),
        task_id: "task-1".to_owned(),
        effect: ApprovalEffect {
            app: "Mail".to_owned(),
            target: "message-42".to_owned(),
            material: "send".to_owned(),
        },
        before_state_digest: "before-1".to_owned(),
    }
}

fn propose(executor: &mut TypedActionExecutor, id: &str) -> holoiroh_wire::ActionApprovalRequest {
    executor
        .propose("session-1", action(id), NOW + 30_000, NOW)
        .unwrap()
}

#[derive(Default)]
struct Primitive {
    calls: usize,
    observations: usize,
    fail_on_observation: Option<usize>,
    fresh: VecDeque<FreshTargetState>,
}

impl PrimitiveAdapter for Primitive {
    type Error = &'static str;

    fn observe(&mut self, _target: &TargetRef) -> Result<FreshTargetState, Self::Error> {
        self.observations += 1;
        if self.fail_on_observation == Some(self.observations) {
            return Err("observation failed");
        }
        self.fresh.pop_front().ok_or("fresh state unavailable")
    }

    fn execute_observe(&mut self, _target: &TargetRef) -> Result<(), Self::Error> {
        self.calls += 1;
        Ok(())
    }

    fn execute_navigation(
        &mut self,
        _target: &TargetRef,
        _action: &NavigationAction,
        bounds: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), Self::Error> {
        assert!(bounds.is_some());
        self.calls += 1;
        Ok(())
    }

    fn execute_focus(&mut self, _target: &TargetRef) -> Result<(), Self::Error> {
        self.calls += 1;
        Ok(())
    }

    fn execute_draft(&mut self, _target: &TargetRef, _text: &str) -> Result<(), Self::Error> {
        self.calls += 1;
        Ok(())
    }

    fn execute_commit(
        &mut self,
        _target: &TargetRef,
        _action: CommitAction,
    ) -> Result<(), Self::Error> {
        self.calls += 1;
        Ok(())
    }
}

fn structured(action: DesktopAction) -> ActionProposal {
    let digest = "a".repeat(64);
    let mut proposal = ActionProposal {
        goal_id: "goal-1".into(),
        intent_digest: digest.clone(),
        run_id: "run-1".into(),
        task_id: "task-1".into(),
        action_id: ActionId("action-1".into()),
        observation: ObservationRef {
            observation_id: "observation-1".into(),
            before_state_digest: digest.clone(),
        },
        target: TargetRef {
            bundle_id: "com.example.App".into(),
            window_id: "window-1".into(),
            element_id: "element-1".into(),
            expected_role: "AXButton".into(),
            expected_title_digest: digest.clone(),
            expected_value_digest: None,
            sensitive: false,
            credential: false,
            resolved: true,
        },
        action,
        proposal_digest: digest,
    };
    proposal.proposal_digest = canonical_proposal_digest(&proposal);
    proposal
}

fn fresh(proposal: &ActionProposal) -> FreshTargetState {
    FreshTargetState {
        bundle_id: proposal.target.bundle_id.clone(),
        window_id: proposal.target.window_id.clone(),
        element_id: proposal.target.element_id.clone(),
        role: proposal.target.expected_role.clone(),
        title_digest: proposal.target.expected_title_digest.clone(),
        value_digest: proposal.target.expected_value_digest.clone(),
        before_state_digest: proposal.observation.before_state_digest.clone(),
        bounds: Some((0, 0, 100, 100)),
    }
}

fn assert_second_observation_stale(mut mutate: impl FnMut(&mut FreshTargetState)) {
    let mut proposal = structured(DesktopAction::Commit(CommitAction::SendMessage));
    proposal.action_id = ActionId(uuid::Uuid::new_v4().to_string());
    proposal.proposal_digest = canonical_proposal_digest(&proposal);
    let first = fresh(&proposal);
    let mut second = first.clone();
    mutate(&mut second);
    let store = Arc::new(Mutex::new(ApprovalStore::new(4)));
    let mut daemon = DaemonActionExecutor::new(store.clone(), 4);
    let mut primitive = Primitive {
        fresh: VecDeque::from([first.clone(), first, second]),
        ..Default::default()
    };
    let request = match daemon
        .issue_approval("session-1", proposal, NOW + 30_000, NOW, &mut primitive)
        .unwrap()
    {
        holoiroh_daemon::action_executor::ExecutionOutcome::ApprovalRequired(request) => request,
        other => panic!("expected approval request, got {other:?}"),
    };
    let response = response_for(&request, ApprovalDecision::Approve);
    store
        .lock()
        .unwrap()
        .route_response(&response, "session-1", Some("task-1"), NOW + 1)
        .unwrap();
    primitive.observations = 0;
    let receipt = daemon
        .execute_routed(&request.approval_id, NOW + 2, &mut primitive)
        .unwrap();
    assert_eq!(
        receipt.outcome,
        holoiroh_daemon::action_executor::ExecutionOutcome::Stale
    );
    assert_eq!(primitive.observations, 2);
    assert_eq!(primitive.calls, 0);
}

fn assert_observation_error_invalidates(fail_on_observation: usize) {
    let mut proposal = structured(DesktopAction::Commit(CommitAction::SendMessage));
    proposal.action_id = ActionId(uuid::Uuid::new_v4().to_string());
    proposal.proposal_digest = canonical_proposal_digest(&proposal);
    let current = fresh(&proposal);
    let path = std::env::temp_dir().join(format!(
        "holoiroh-observation-error-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let logger = Arc::new(AuditLogger::new(&path).unwrap());
    let store = Arc::new(Mutex::new(ApprovalStore::new(4)));
    let mut daemon = DaemonActionExecutor::new(store.clone(), 4).with_audit(logger);
    let mut primitive = Primitive {
        fresh: VecDeque::from([current.clone(), current.clone(), current]),
        ..Default::default()
    };
    let request = match daemon
        .issue_approval("session-1", proposal, NOW + 30_000, NOW, &mut primitive)
        .unwrap()
    {
        holoiroh_daemon::action_executor::ExecutionOutcome::ApprovalRequired(request) => request,
        other => panic!("expected approval request, got {other:?}"),
    };
    let response = response_for(&request, ApprovalDecision::Approve);
    store
        .lock()
        .unwrap()
        .route_response(&response, "session-1", Some("task-1"), NOW + 1)
        .unwrap();
    primitive.observations = 0;
    primitive.fail_on_observation = Some(fail_on_observation);
    assert_eq!(
        daemon.execute_routed(&request.approval_id, NOW + 2, &mut primitive),
        Err("observation failed")
    );
    assert_eq!(primitive.calls, 0);
    assert_eq!(
        store
            .lock()
            .unwrap()
            .route_response(&response, "session-1", Some("task-1"), NOW + 3,),
        Err(ApprovalError::Replay)
    );
    let bytes = fs::read_to_string(&path).unwrap();
    assert!(bytes.contains("observation_error"));
    fs::remove_file(path).unwrap();
}

fn sign_client_envelope(
    envelope: &mut TaskEnvelope<ClientMessage>,
    client: &iroh::SecretKey,
    daemon: &iroh::PublicKey,
) {
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            client.public().as_bytes(),
            daemon.as_bytes(),
        )
        .unwrap();
    envelope.signature = Some(encode_ed25519_signature(&client.sign(&payload).to_bytes()));
}

fn main() {
    let mut executor = TypedActionExecutor::new(32);
    let request = propose(&mut executor, "happy");
    let response = response_for(&request, ApprovalDecision::Approve);
    let mut state = State {
        values: vec![Ok("before-1"), Ok("before-1")],
    };
    let mut backend = Backend::default();
    executor
        .execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut state,
            &mut backend,
        )
        .unwrap();
    assert_eq!(backend.effects, [ActionId("happy".to_owned())]);
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 2,
            &mut State::default(),
            &mut backend
        ),
        Err(ActionExecutionError::UnknownProposal)
    );
    assert_eq!(backend.effects.len(), 1);
    assert_eq!(
        executor.propose("session-1", action("happy"), NOW + 1, NOW),
        Err(ApprovalError::Replay)
    );

    let request = propose(&mut executor, "tampered");
    let mut response = response_for(&request, ApprovalDecision::Approve);
    response.action_id = ActionId("other".to_owned());
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut State::default(),
            &mut backend
        ),
        Err(ActionExecutionError::ProposalBindingMismatch)
    );
    assert_eq!(backend.effects.len(), 1);

    let request = propose(&mut executor, "denied");
    let response = response_for(&request, ApprovalDecision::Deny);
    let mut state = State {
        values: vec![Ok("before-1"), Ok("before-1")],
    };
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut state,
            &mut backend
        ),
        Err(ActionExecutionError::NotApproved(ApprovalOutcome::Denied))
    );
    assert_eq!(backend.effects.len(), 1);

    let request = propose(&mut executor, "expired");
    let response = response_for(&request, ApprovalDecision::Approve);
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            request.expires_at + 1,
            &mut State::default(),
            &mut backend
        ),
        Err(ActionExecutionError::Approval(ApprovalError::Expired))
    );
    assert_eq!(backend.effects.len(), 1);

    let request = propose(&mut executor, "stale");
    let response = response_for(&request, ApprovalDecision::Approve);
    let mut state = State {
        values: vec![Ok("changed")],
    };
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut state,
            &mut backend
        ),
        Err(ActionExecutionError::StateChanged)
    );
    assert_eq!(backend.effects.len(), 1);

    let request = propose(&mut executor, "toctou");
    let response = response_for(&request, ApprovalDecision::Approve);
    let mut state = State {
        values: vec![Ok("before-1"), Ok("changed")],
    };
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut state,
            &mut backend
        ),
        Err(ActionExecutionError::StateChanged)
    );
    assert_eq!(backend.effects.len(), 1);

    let request = propose(&mut executor, "state-error");
    let response = response_for(&request, ApprovalDecision::Approve);
    let mut state = State {
        values: vec![Err("state unavailable")],
    };
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut state,
            &mut backend
        ),
        Err(ActionExecutionError::State("state unavailable"))
    );
    assert_eq!(backend.effects.len(), 1);

    let request = propose(&mut executor, "backend-error");
    let response = response_for(&request, ApprovalDecision::Approve);
    let mut state = State {
        values: vec![Ok("before-1"), Ok("before-1")],
    };
    backend.fail = true;
    assert_eq!(
        executor.execute(
            &response,
            "session-1",
            Some("task-1"),
            NOW + 1,
            &mut state,
            &mut backend
        ),
        Err(ActionExecutionError::Backend("backend failed"))
    );
    assert_eq!(backend.effects.len(), 2);
    assert_eq!(
        executor.propose("session-1", action("backend-error"), NOW + 2, NOW + 1),
        Err(ApprovalError::Replay)
    );

    let variants = [
        DesktopAction::Observe,
        DesktopAction::Navigate(NavigationAction::SemanticActivate),
        DesktopAction::Navigate(NavigationAction::CoordinateActivate { x: 10, y: 10 }),
        DesktopAction::Navigate(NavigationAction::Scroll {
            horizontal: 0,
            vertical: 10,
        }),
        DesktopAction::Focus,
        DesktopAction::DraftText {
            text: "draft".into(),
        },
    ];
    for variant in variants {
        let proposal = structured(variant);
        let mut primitive = Primitive {
            fresh: VecDeque::from([fresh(&proposal)]),
            ..Default::default()
        };
        let receipt = execute_primitive(&proposal, &mut primitive).unwrap();
        assert_eq!(
            receipt.outcome,
            holoiroh_daemon::action_executor::ExecutionOutcome::Executed
        );
        assert_eq!(primitive.calls, 1);
    }

    let commit = structured(DesktopAction::Commit(CommitAction::SendMessage));
    let mut primitive = Primitive {
        fresh: VecDeque::from([fresh(&commit)]),
        ..Default::default()
    };
    let receipt = execute_primitive(&commit, &mut primitive).unwrap();
    assert_eq!(receipt.risk, ActionRisk::ExternalCommitment);
    assert_eq!(primitive.calls, 0);

    let shared = Arc::new(Mutex::new(ApprovalStore::new(32)));
    let daemon = Arc::new(Mutex::new(DaemonActionExecutor::new(shared.clone(), 32)));
    let approved_commit = structured(DesktopAction::Commit(CommitAction::SendMessage));
    let mut primitive = Primitive {
        fresh: VecDeque::from([
            fresh(&approved_commit),
            fresh(&approved_commit),
            fresh(&approved_commit),
        ]),
        ..Default::default()
    };
    let request = match daemon
        .lock()
        .unwrap()
        .issue_approval(
            "session-1",
            approved_commit.clone(),
            NOW + 30_000,
            NOW,
            &mut primitive,
        )
        .unwrap()
    {
        holoiroh_daemon::action_executor::ExecutionOutcome::ApprovalRequired(request) => request,
        other => panic!("expected approval request, got {other:?}"),
    };
    assert_eq!(primitive.calls, 0);
    assert_eq!(primitive.observations, 1);
    primitive.observations = 0;
    let response = response_for(&request, ApprovalDecision::Approve);
    let client = iroh::SecretKey::generate();
    let daemon_key = iroh::SecretKey::generate();
    let mut envelope = TaskEnvelope::<ClientMessage>::wrap(
        "session-1".into(),
        Some("task-1".into()),
        0,
        ClientMessage::ApprovalResponse { response },
    );
    let signing_payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            client.public().as_bytes(),
            daemon_key.public().as_bytes(),
        )
        .unwrap();
    envelope.signature = Some(encode_ed25519_signature(
        &client.sign(&signing_payload).to_bytes(),
    ));
    verify_client_envelope_for_probing(&envelope, &client.public(), &daemon_key.public()).unwrap();

    let mut wrong_protocol = envelope.clone();
    wrong_protocol.protocol_version += 1;
    sign_client_envelope(&mut wrong_protocol, &client, &daemon_key.public());
    verify_client_envelope_for_probing(&wrong_protocol, &client.public(), &daemon_key.public())
        .unwrap();
    assert!(
        admit_post_signature_envelope(
            &wrong_protocol,
            &wrong_protocol.payload,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let mut wrong_type = envelope.clone();
    wrong_type.message_type = "prompt".into();
    sign_client_envelope(&mut wrong_type, &client, &daemon_key.public());
    assert!(
        admit_post_signature_envelope(
            &wrong_type,
            &wrong_type.payload,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let mut wrong_session = envelope.clone();
    wrong_session.session_id = "session-2".into();
    sign_client_envelope(&mut wrong_session, &client, &daemon_key.public());
    assert!(
        admit_post_signature_envelope(
            &wrong_session,
            &wrong_session.payload,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let mut wrong_task = envelope.clone();
    wrong_task.task_id = Some("task-2".into());
    sign_client_envelope(&mut wrong_task, &client, &daemon_key.public());
    assert!(
        admit_post_signature_envelope(
            &wrong_task,
            &wrong_task.payload,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let mut wrong_first_sequence = envelope.clone();
    wrong_first_sequence.sequence_number = 1;
    sign_client_envelope(&mut wrong_first_sequence, &client, &daemon_key.public());
    assert!(
        admit_post_signature_envelope(
            &wrong_first_sequence,
            &wrong_first_sequence.payload,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let mut expired = envelope.clone();
    expired.expires_at = 0;
    sign_client_envelope(&mut expired, &client, &daemon_key.public());
    assert!(
        admit_post_signature_envelope(
            &expired,
            &expired.payload,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let restricted_message = ClientMessage::Redirect {
        text: "must not invalidate approval state".into(),
    };
    let mut restricted = TaskEnvelope::<ClientMessage>::wrap(
        "session-1".into(),
        Some("task-1".into()),
        0,
        restricted_message.clone(),
    );
    sign_client_envelope(&mut restricted, &client, &daemon_key.public());
    assert!(
        admit_post_signature_envelope(
            &restricted,
            &restricted_message,
            "session-1",
            true,
            &mut InboundEnvelopeState::new(),
            ExecutionMode::Restricted,
            &daemon,
            NOW + 1,
        )
        .is_err()
    );

    let mut inbound = InboundEnvelopeState::new();
    admit_post_signature_envelope(
        &envelope,
        &envelope.payload,
        "session-1",
        true,
        &mut inbound,
        ExecutionMode::Restricted,
        &daemon,
        NOW + 1,
    )
    .unwrap();
    assert!(
        admit_post_signature_envelope(
            &envelope,
            &envelope.payload,
            "session-1",
            false,
            &mut inbound,
            ExecutionMode::Restricted,
            &daemon,
            NOW + 2,
        )
        .is_err()
    );
    let result = daemon
        .lock()
        .unwrap()
        .execute_routed(&request.approval_id, NOW + 2, &mut primitive)
        .unwrap();
    assert_eq!(
        result.outcome,
        holoiroh_daemon::action_executor::ExecutionOutcome::Executed
    );
    assert_eq!(primitive.calls, 1);
    assert_eq!(primitive.observations, 2);
    let replay = daemon
        .lock()
        .unwrap()
        .execute_routed(&request.approval_id, NOW + 3, &mut primitive)
        .unwrap();
    assert_eq!(
        replay.outcome,
        holoiroh_daemon::action_executor::ExecutionOutcome::Unsupported
    );
    assert_eq!(primitive.calls, 1);

    let original = structured(DesktopAction::DraftText {
        text: "draft".into(),
    });
    let mut mutations = Vec::new();
    let mut changed = original.clone();
    changed.goal_id = "goal-2".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.intent_digest = "b".repeat(64);
    mutations.push(changed);
    let mut changed = original.clone();
    changed.run_id = "run-2".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.task_id = "task-2".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.action_id = ActionId("action-2".into());
    mutations.push(changed);
    let mut changed = original.clone();
    changed.observation.observation_id = "observation-2".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.observation.before_state_digest = "b".repeat(64);
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.bundle_id = "com.example.Other".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.window_id = "window-2".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.element_id = "element-2".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.expected_role = "AXTextField".into();
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.expected_title_digest = "b".repeat(64);
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.expected_value_digest = Some("b".repeat(64));
    mutations.push(changed);
    let mut changed = original.clone();
    changed.target.sensitive = true;
    mutations.push(changed);
    let mut changed = original.clone();
    changed.action = DesktopAction::DraftText {
        text: "changed".into(),
    };
    mutations.push(changed);
    assert!(
        mutations
            .iter()
            .all(|proposal| validate_proposal(proposal).is_err())
    );

    let mut sensitive = structured(DesktopAction::Commit(CommitAction::Purchase));
    sensitive.target.sensitive = true;
    sensitive.proposal_digest = canonical_proposal_digest(&sensitive);
    let sensitive_store = Arc::new(Mutex::new(ApprovalStore::new(4)));
    let mut sensitive_daemon = DaemonActionExecutor::new(sensitive_store, 4);
    let mut sensitive_primitive = Primitive {
        fresh: VecDeque::from([fresh(&sensitive)]),
        ..Default::default()
    };
    assert_eq!(
        sensitive_daemon
            .issue_approval(
                "session-1",
                sensitive,
                NOW + 30_000,
                NOW,
                &mut sensitive_primitive,
            )
            .unwrap(),
        holoiroh_daemon::action_executor::ExecutionOutcome::Unsupported
    );
    assert_eq!(sensitive_primitive.observations, 0);
    assert_eq!(sensitive_primitive.calls, 0);

    let mut credential = structured(DesktopAction::DraftText {
        text: "secret".into(),
    });
    credential.target.credential = true;
    assert_eq!(
        classify_action(&credential.action, &credential.target),
        ActionRisk::CredentialBoundary
    );
    let mut primitive = Primitive {
        fresh: VecDeque::from([fresh(&credential)]),
        ..Default::default()
    };
    let _ = execute_primitive(&credential, &mut primitive).unwrap();
    assert_eq!(primitive.calls, 0);

    let mut unresolved = structured(DesktopAction::Focus);
    unresolved.target.resolved = false;
    assert_eq!(
        classify_action(&unresolved.action, &unresolved.target),
        ActionRisk::Unsupported
    );

    let stale = structured(DesktopAction::Focus);
    let mut stale_state = fresh(&stale);
    stale_state.role = "AXTextField".into();
    let mut primitive = Primitive {
        fresh: VecDeque::from([stale_state]),
        ..Default::default()
    };
    let _ = execute_primitive(&stale, &mut primitive).unwrap();
    assert_eq!(primitive.calls, 0);

    assert_second_observation_stale(|state| state.element_id = "element-2".into());
    assert_second_observation_stale(|state| state.bundle_id = "com.example.Other".into());
    assert_second_observation_stale(|state| state.window_id = "window-2".into());
    assert_second_observation_stale(|state| state.role = "AXTextField".into());
    assert_second_observation_stale(|state| state.title_digest = "b".repeat(64));
    assert_second_observation_stale(|state| state.value_digest = Some("b".repeat(64)));
    assert_second_observation_stale(|state| state.before_state_digest = "b".repeat(64));
    assert_second_observation_stale(|state| state.bounds = Some((1, 1, 99, 99)));
    assert_observation_error_invalidates(1);
    assert_observation_error_invalidates(2);

    let audit_path = std::env::temp_dir().join(format!(
        "holoiroh-typed-action-audit-{}.jsonl",
        uuid::Uuid::new_v4()
    ));
    let logger = Arc::new(AuditLogger::new(&audit_path).unwrap());
    let audit_store = Arc::new(Mutex::new(ApprovalStore::new(8)));
    let audit_daemon = Arc::new(Mutex::new(
        DaemonActionExecutor::new(audit_store.clone(), 8).with_audit(logger),
    ));
    let mut audit_proposal = structured(DesktopAction::Commit(CommitAction::SubmitForm));
    audit_proposal.action_id = ActionId("audit-action".into());
    audit_proposal.target.element_id = "SCREEN_CONTENT_MARKER_NEVER_LOG".into();
    audit_proposal.proposal_digest = canonical_proposal_digest(&audit_proposal);
    let mut audit_primitive = Primitive {
        fresh: VecDeque::from([
            fresh(&audit_proposal),
            fresh(&audit_proposal),
            fresh(&audit_proposal),
        ]),
        ..Default::default()
    };
    let audit_request = match audit_daemon
        .lock()
        .unwrap()
        .issue_approval(
            "session-1",
            audit_proposal,
            NOW + 30_000,
            NOW,
            &mut audit_primitive,
        )
        .unwrap()
    {
        holoiroh_daemon::action_executor::ExecutionOutcome::ApprovalRequired(request) => request,
        other => panic!("expected audit approval request, got {other:?}"),
    };
    let audit_response = response_for(&audit_request, ApprovalDecision::Approve);
    let mut audit_envelope = TaskEnvelope::<ClientMessage>::wrap(
        "session-1".into(),
        Some("task-1".into()),
        0,
        ClientMessage::ApprovalResponse {
            response: audit_response,
        },
    );
    let audit_signing_payload = audit_envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            client.public().as_bytes(),
            daemon_key.public().as_bytes(),
        )
        .unwrap();
    audit_envelope.signature = Some(encode_ed25519_signature(
        &client.sign(&audit_signing_payload).to_bytes(),
    ));
    verify_client_envelope_for_probing(&audit_envelope, &client.public(), &daemon_key.public())
        .unwrap();
    admit_post_signature_envelope(
        &audit_envelope,
        &audit_envelope.payload,
        "session-1",
        true,
        &mut InboundEnvelopeState::new(),
        ExecutionMode::Restricted,
        &audit_daemon,
        NOW + 1,
    )
    .unwrap();
    audit_primitive.observations = 0;
    audit_daemon
        .lock()
        .unwrap()
        .execute_routed(&audit_request.approval_id, NOW + 2, &mut audit_primitive)
        .unwrap();
    assert_eq!(audit_primitive.observations, 2);
    assert_eq!(audit_primitive.calls, 1);
    let audit_bytes = fs::read_to_string(&audit_path).unwrap();
    assert!(audit_bytes.contains("audit-action"));
    assert!(audit_bytes.contains("approval_required"));
    assert!(audit_bytes.contains("executed"));
    assert!(!audit_bytes.contains("SCREEN_CONTENT_MARKER_NEVER_LOG"));
    assert!(!audit_bytes.contains("submit_form"));
    fs::remove_file(audit_path).unwrap();

    println!("typed_action_executor_probe: OK");
}
