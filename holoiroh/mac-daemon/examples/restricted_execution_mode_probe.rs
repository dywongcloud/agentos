use holoiroh_daemon::control_channel::verify_client_envelope_for_probing;
use holoiroh_daemon::execution_mode::{
    ExecutionMode, LEGACY_HOLO_CAPABILITIES, RESTRICTED_CAPABILITIES,
};
use holoiroh_wire::{ClientMessage, MouseButton, RemoteControlEvent, ServerMessage, TaskEnvelope};

fn main() {
    assert_eq!(ExecutionMode::default(), ExecutionMode::Restricted);
    assert_eq!(
        ExecutionMode::Restricted.capabilities(),
        RESTRICTED_CAPABILITIES
    );
    assert_eq!(
        ExecutionMode::LegacyHolo.capabilities(),
        LEGACY_HOLO_CAPABILITIES
    );

    let rejected = [
        ClientMessage::Prompt {
            text: "private".into(),
        },
        ClientMessage::VoiceTranscript {
            text: "private".into(),
        },
        ClientMessage::Redirect {
            text: "private".into(),
        },
        ClientMessage::Resume,
        ClientMessage::InputResponse {
            request_id: "consent".into(),
            selected_option: "Allow once".into(),
        },
    ];
    assert!(
        rejected
            .iter()
            .all(|message| !ExecutionMode::Restricted.admits(message))
    );
    assert!(
        rejected
            .iter()
            .all(|message| ExecutionMode::LegacyHolo.admits(message))
    );

    let preserved = [
        ClientMessage::PlanTask {
            request_id: "plan".into(),
            goal: "goal".into(),
        },
        ClientMessage::ClarifyRequest {
            prompt: "clarify".into(),
        },
        ClientMessage::ProcessDocument {
            request_id: "doc".into(),
            filename: "a.txt".into(),
            data_base64: String::new(),
            mode: "text".into(),
        },
        ClientMessage::AnalyzeImage {
            request_id: "image".into(),
            image_data_base64: String::new(),
            prompt: "observe".into(),
        },
        ClientMessage::TranscribeAudio {
            request_id: "audio".into(),
            audio_data_base64: String::new(),
            format: "wav".into(),
        },
        ClientMessage::RequestSpeech {
            request_id: "speech".into(),
            text: "speak".into(),
            voice: "serena".into(),
        },
        ClientMessage::Stop { context_id: None },
        ClientMessage::Pause,
    ];
    assert!(
        preserved
            .iter()
            .all(|message| ExecutionMode::Restricted.admits(message))
    );

    let client = iroh::SecretKey::generate();
    let daemon = iroh::SecretKey::generate();
    let remote = ClientMessage::RemoteControl {
        event: RemoteControlEvent::Click {
            x: 0.5,
            y: 0.5,
            button: MouseButton::Left,
            count: 1,
        },
    };
    let mut envelope =
        TaskEnvelope::<ClientMessage>::wrap("session".into(), None, 0, remote.clone());
    let payload = envelope
        .signing_payload(
            holoiroh_wire::EnvelopeDirection::ClientToDaemon,
            client.public().as_bytes(),
            daemon.public().as_bytes(),
        )
        .unwrap();
    envelope.signature = Some(holoiroh_wire::encode_ed25519_signature(
        &client.sign(&payload).to_bytes(),
    ));
    verify_client_envelope_for_probing(&envelope, &client.public(), &daemon.public()).unwrap();
    assert!(ExecutionMode::Restricted.admits(&remote));

    let greeting = ServerMessage::greeting(
        "control channel ready",
        ExecutionMode::Restricted.wire_name(),
        ExecutionMode::Restricted.capabilities().iter().copied(),
    );
    let greeting_json = serde_json::to_string(&greeting).unwrap();
    assert_eq!(
        greeting_json,
        r#"{"type":"status","text":"control channel ready","execution_mode":"restricted","capabilities":["plan_task","clarify_request","observation_media","signed_remote_control"]}"#
    );
    let decoded: ServerMessage = serde_json::from_str(&greeting_json).unwrap();
    assert!(matches!(
        decoded,
        ServerMessage::Status {
            text: Some(text),
            execution_mode: Some(mode),
            capabilities: Some(capabilities),
        } if text == "control channel ready"
            && mode == "restricted"
            && capabilities == RESTRICTED_CAPABILITIES
    ));

    println!("restricted_execution_mode_probe: OK");
}
