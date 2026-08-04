use holoiroh_daemon::approval::{
    ApprovalError, ApprovalOutcome, ApprovalStore, MAX_APPROVAL_TTL_MS, response_for,
};
use holoiroh_daemon::control_channel::{
    ActionApprovalRequest, ActionApprovalResponse, ActionId, ApprovalDecision, ApprovalEffect,
    ApprovalLifecycleInvalidation, ApprovalRisk, ClientMessage, EnvelopeDirection, ServerMessage,
    TaskEnvelope, encode_ed25519_signature, invalidate_approvals_for_lifecycle,
    verify_client_envelope_for_probing,
};

const NOW: u64 = 1_800_000_000_000;

fn issue(store: &mut ApprovalStore, suffix: &str) -> ActionApprovalRequest {
    store
        .issue(
            "session-1",
            ActionId(format!("action-{suffix}")),
            "run-1",
            "task-1",
            ApprovalEffect {
                app: "Mail".into(),
                target: "recipient@example.com".into(),
                material: "send message".into(),
            },
            "before-1",
            NOW + 600_000,
            NOW,
        )
        .unwrap()
}

fn resolve(
    store: &mut ApprovalStore,
    response: &ActionApprovalResponse,
    session: &str,
    task: Option<&str>,
    before: &str,
    now: u64,
) -> Result<ApprovalOutcome, ApprovalError> {
    store.resolve(response, session, task, before, now)
}

fn main() {
    let mut store = ApprovalStore::new(32);
    let request = issue(&mut store, "wire");
    assert_eq!(request.risk, ApprovalRisk::Critical);
    assert_eq!(request.expires_at, NOW + MAX_APPROVAL_TTL_MS);
    let request_message = ServerMessage::ApprovalRequest {
        request: request.clone(),
    };
    let request_json = serde_json::to_string(&request_message).unwrap();
    assert_eq!(
        serde_json::from_str::<ServerMessage>(&request_json).unwrap(),
        request_message
    );
    assert!(request_json.contains("\"type\":\"approval_request\""));
    assert!(request_json.contains("\"approval_id\":"));
    assert!(!request_json.contains("\"request\":"));
    let response = response_for(&request, ApprovalDecision::Approve);
    let response_message = ClientMessage::ApprovalResponse {
        response: response.clone(),
    };
    let response_json = serde_json::to_string(&response_message).unwrap();
    assert_eq!(
        serde_json::from_str::<ClientMessage>(&response_json).unwrap(),
        response_message
    );
    assert!(response_json.contains("\"type\":\"approval_response\""));
    assert!(response_json.contains("\"proposal_digest\":"));
    assert!(!response_json.contains("\"response\":"));
    assert_eq!(
        resolve(
            &mut store,
            &response,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Ok(ApprovalOutcome::Approved)
    );
    assert_eq!(
        resolve(
            &mut store,
            &response,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 2
        ),
        Err(ApprovalError::Replay)
    );

    let request = issue(&mut store, "binding");
    let original = response_for(&request, ApprovalDecision::Approve);
    for mut tampered in [original.clone(), original.clone()] {
        if tampered.action_id == original.action_id {
            tampered.action_id = ActionId("other".into());
        }
        assert_eq!(
            resolve(
                &mut store,
                &tampered,
                "session-1",
                Some("task-1"),
                "before-1",
                NOW + 1
            ),
            Err(ApprovalError::BindingMismatch)
        );
        break;
    }
    let mut digest_tamper = original.clone();
    digest_tamper.proposal_digest.push('0');
    assert_eq!(
        resolve(
            &mut store,
            &digest_tamper,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Err(ApprovalError::BindingMismatch)
    );
    assert_eq!(
        resolve(
            &mut store,
            &original,
            "wrong-session",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Err(ApprovalError::SessionMismatch)
    );
    assert_eq!(
        resolve(
            &mut store,
            &original,
            "session-1",
            Some("wrong-task"),
            "before-1",
            NOW + 1
        ),
        Err(ApprovalError::TaskMismatch)
    );
    assert_eq!(
        resolve(
            &mut store,
            &original,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Ok(ApprovalOutcome::Approved)
    );

    let request = issue(&mut store, "stale");
    let response = response_for(&request, ApprovalDecision::Approve);
    assert_eq!(
        resolve(
            &mut store,
            &response,
            "session-1",
            Some("task-1"),
            "changed",
            NOW + 1
        ),
        Err(ApprovalError::StaleBeforeState)
    );
    assert_eq!(
        resolve(
            &mut store,
            &response,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 2
        ),
        Err(ApprovalError::Replay)
    );

    let request = issue(&mut store, "expired");
    let response = response_for(&request, ApprovalDecision::Approve);
    assert_eq!(
        resolve(
            &mut store,
            &response,
            "session-1",
            Some("task-1"),
            "before-1",
            request.expires_at + 1
        ),
        Err(ApprovalError::Expired)
    );

    for (decision, outcome) in [
        (ApprovalDecision::Deny, ApprovalOutcome::Denied),
        (ApprovalDecision::Cancel, ApprovalOutcome::Canceled),
    ] {
        let request = issue(&mut store, &format!("{decision:?}"));
        let response = response_for(&request, decision);
        assert_eq!(
            resolve(
                &mut store,
                &response,
                "session-1",
                Some("task-1"),
                "before-1",
                NOW + 1
            ),
            Ok(outcome)
        );
        assert_eq!(
            resolve(
                &mut store,
                &response,
                "session-1",
                Some("task-1"),
                "before-1",
                NOW + 2
            ),
            Err(ApprovalError::Replay)
        );
    }

    let request = issue(&mut store, "task-cancel");
    let response = response_for(&request, ApprovalDecision::Approve);
    assert!(store.cancel_task("run-1", "task-1") > 0);
    assert_eq!(
        resolve(
            &mut store,
            &response,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Err(ApprovalError::Replay)
    );

    for (lifecycle, invalidation) in [
        (
            "stop",
            ApprovalLifecycleInvalidation::Stop {
                session_id: "session-1",
            },
        ),
        (
            "pause",
            ApprovalLifecycleInvalidation::Pause {
                session_id: "session-1",
            },
        ),
        (
            "redirect",
            ApprovalLifecycleInvalidation::Redirect {
                session_id: "session-1",
            },
        ),
        (
            "disconnect",
            ApprovalLifecycleInvalidation::Disconnect {
                session_id: "session-1",
            },
        ),
    ] {
        let mut lifecycle_store = ApprovalStore::new(4);
        let request = issue(&mut lifecycle_store, lifecycle);
        let response = response_for(&request, ApprovalDecision::Approve);
        assert_eq!(
            invalidate_approvals_for_lifecycle(&mut lifecycle_store, invalidation),
            1
        );
        assert_eq!(
            resolve(
                &mut lifecycle_store,
                &response,
                "session-1",
                Some("task-1"),
                "before-1",
                NOW + 1
            ),
            Err(ApprovalError::Replay),
            "{lifecycle} must invalidate the pending approval"
        );
    }

    let mut terminal_store = ApprovalStore::new(4);
    let terminal_request = issue(&mut terminal_store, "terminal");
    let terminal_response = response_for(&terminal_request, ApprovalDecision::Approve);
    assert_eq!(
        invalidate_approvals_for_lifecycle(
            &mut terminal_store,
            ApprovalLifecycleInvalidation::Terminal { task_id: "task-1" },
        ),
        1
    );
    assert_eq!(
        resolve(
            &mut terminal_store,
            &terminal_response,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Err(ApprovalError::Replay),
        "terminal task event must invalidate the pending approval"
    );

    let client = iroh::SecretKey::generate();
    let daemon = iroh::SecretKey::generate();
    let request = issue(&mut store, "signed");
    let response = response_for(&request, ApprovalDecision::Approve);
    let mut envelope = TaskEnvelope::<ClientMessage>::wrap(
        "session-1".into(),
        Some("task-1".into()),
        0,
        ClientMessage::ApprovalResponse { response },
    );
    assert!(
        verify_client_envelope_for_probing(&envelope, &client.public(), &daemon.public()).is_err()
    );
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            client.public().as_bytes(),
            daemon.public().as_bytes(),
        )
        .unwrap();
    envelope.signature = Some(encode_ed25519_signature(&client.sign(&payload).to_bytes()));
    verify_client_envelope_for_probing(&envelope, &client.public(), &daemon.public()).unwrap();
    let signed_original = envelope.clone();
    if let ClientMessage::ApprovalResponse { response } = &mut envelope.payload {
        response.proposal_digest.push('0');
    }
    assert!(
        verify_client_envelope_for_probing(&envelope, &client.public(), &daemon.public()).is_err()
    );
    let ClientMessage::ApprovalResponse { response } = &signed_original.payload else {
        unreachable!()
    };
    assert_eq!(
        resolve(
            &mut store,
            response,
            "session-1",
            Some("task-1"),
            "before-1",
            NOW + 1
        ),
        Ok(ApprovalOutcome::Approved)
    );

    println!("typed_action_approval_probe: OK");
}
