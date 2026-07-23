//! Pure-logic CI witness for the clarify wire messages
//! (`ClientMessage::ClarifyRequest` + `ServerMessage::ClarifyQuestions` +
//! `ClarifyingQuestion`). Deterministic, no daemon needed -- it pins the exact
//! JSON the iOS app must produce/parse and confirms the new variants leave every
//! existing message kind round-tripping unchanged.
//!
//! Run with `cargo run --example clarify_wire_probe -p holoiroh-daemon`.

use holoiroh_daemon::control_channel::{ClientMessage, ServerMessage};
use holoiroh_wire::ClarifyingQuestion;

fn main() {
    let request = ClientMessage::ClarifyRequest {
        prompt: "send a message to the team".to_string(),
    };
    let rj = serde_json::to_string(&request).expect("serialize request");
    println!("clarify_request -> {rj}");
    assert!(rj.contains("\"type\":\"clarify_request\""), "wrong type tag: {rj}");
    assert!(rj.contains("\"prompt\":\"send a message to the team\""), "prompt missing: {rj}");
    let back: ClientMessage = serde_json::from_str(&rj).expect("deserialize request");
    assert_eq!(back, request);

    let questions = ServerMessage::clarify_questions(vec![
        ClarifyingQuestion {
            question: "Which app?".to_string(),
            options: vec!["Slack".to_string(), "Email".to_string()],
        },
        ClarifyingQuestion {
            question: "Which team?".to_string(),
            options: vec!["Engineering".to_string()],
        },
    ]);
    let qj = serde_json::to_string(&questions).expect("serialize questions");
    println!("clarify_questions -> {qj}");
    assert!(qj.contains("\"type\":\"clarify_questions\""), "wrong type tag: {qj}");
    assert!(qj.contains("\"question\":\"Which app?\"") && qj.contains("Slack"), "options missing: {qj}");
    let back_q: ServerMessage = serde_json::from_str(&qj).expect("deserialize questions");
    assert_eq!(back_q, questions);

    let empty = ServerMessage::clarify_questions(Vec::new());
    let ej = serde_json::to_string(&empty).expect("serialize empty");
    assert!(ej.contains("\"questions\":[]"), "empty shape: {ej}");

    let decoded: ServerMessage = serde_json::from_str(
        r#"{"type":"clarify_questions","questions":[{"question":"Q?","options":["a","b"]}]}"#,
    )
    .expect("decode canonical clarify_questions");
    assert!(matches!(decoded, ServerMessage::ClarifyQuestions { .. }));

    for existing in [
        ServerMessage::ack(),
        ServerMessage::status("connected"),
        ServerMessage::error("boom"),
        ServerMessage::current_ticket("iroh-live:abc/holoiroh"),
        ServerMessage::task_done("completed", None),
    ] {
        let j = serde_json::to_string(&existing).expect("serialize existing");
        let round: ServerMessage = serde_json::from_str(&j).expect("deserialize existing");
        assert_eq!(round, existing);
        assert!(!j.contains("clarify_questions"), "existing kind polluted: {j}");
    }

    println!(
        "clarify_wire_probe: OK -- ClarifyRequest/ClarifyQuestions round-trip as the iOS wire expects and existing kinds are unaffected."
    );
}
