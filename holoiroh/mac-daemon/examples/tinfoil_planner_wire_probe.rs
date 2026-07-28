//! Pure-logic CI witness for the planner wire messages (`ClientMessage::PlanTask` +
//! `ServerMessage::PlanReady`/`PlanFailed`) and `tinfoil_planner`'s tool schema shape + empty-goal
//! validation. No network call: the schema is a static JSON value and the empty-goal rejection
//! happens before any request is built.
//!
//!   cargo run --example tinfoil_planner_wire_probe -p holoiroh-daemon

use holoiroh_daemon::control_channel::{ClientMessage, ServerMessage};
use holoiroh_daemon::tinfoil_planner::{plan_task, tool_schema};

#[tokio::main]
async fn main() {
    let request = ClientMessage::PlanTask {
        request_id: "req-5".to_string(),
        goal: "reply to the last email from Sam and archive it".to_string(),
    };
    let rj = serde_json::to_string(&request).expect("serialize request");
    assert!(rj.contains("\"type\":\"plan_task\""), "wrong type tag: {rj}");
    let back: ClientMessage = serde_json::from_str(&rj).expect("deserialize request");
    assert_eq!(back, request);

    let ready = ServerMessage::PlanReady {
        request_id: "req-5".to_string(),
        steps: vec![
            "Desktop action: open Mail and find the last email from Sam".to_string(),
            "Desktop action: reply and archive".to_string(),
        ],
    };
    let rdj = serde_json::to_string(&ready).expect("serialize ready");
    assert!(rdj.contains("\"type\":\"plan_ready\""), "wrong type tag: {rdj}");
    let back_ready: ServerMessage = serde_json::from_str(&rdj).expect("deserialize ready");
    assert_eq!(back_ready, ready);

    let failed = ServerMessage::PlanFailed {
        request_id: "req-5".to_string(),
        error: "no TINFOIL_API_KEY configured".to_string(),
    };
    let fj = serde_json::to_string(&failed).expect("serialize failed");
    assert!(fj.contains("\"type\":\"plan_failed\""), "wrong type tag: {fj}");
    println!("wire round-trip: OK");

    // Tool schema shape: 4 tools, each a real OpenAI-style function definition.
    let schema = tool_schema();
    let tools = schema.as_array().expect("tool_schema must be a JSON array");
    assert_eq!(tools.len(), 4, "expected exactly 4 tools, got {}: {schema}", tools.len());
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("every tool needs function.name"))
        .collect();
    assert_eq!(
        names,
        vec!["start_desktop_task", "process_document", "analyze_image", "transcribe_audio"],
        "tool names/order drifted from the ComputerUseExecutor + tinfoil_* capability mapping"
    );
    for tool in tools {
        assert_eq!(tool["type"], "function", "every tool must be type=function: {tool}");
        assert!(
            tool["function"]["parameters"]["required"].is_array(),
            "every tool needs a required-params array: {tool}"
        );
    }
    println!("tool_schema: OK -- 4 tools, correctly shaped");

    // Empty-goal client-side validation -- no network reached.
    let err = plan_task("fake-key", "")
        .await
        .expect_err("empty goal must be rejected before any network call");
    println!("empty goal -> {err}");

    let err = plan_task("fake-key", "   \n  ")
        .await
        .expect_err("whitespace-only goal must be rejected before any network call");
    println!("whitespace-only goal -> {err}");

    println!(
        "tinfoil_planner_wire_probe: OK -- wire shapes round-trip, tool schema is well-formed, empty goals are rejected before any network call."
    );
}
