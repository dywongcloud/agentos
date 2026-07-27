use std::time::Duration;

use holoiroh_daemon::holo_bridge::a2a_client::A2aClient;
use holoiroh_daemon::holo_bridge::control::{ControlEvent, ControlMessage, HoloControlBridge};
use tokio::sync::mpsc;

fn unreachable_bridge() -> (HoloControlBridge, mpsc::UnboundedReceiver<ControlEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let client = A2aClient::new("http://192.0.2.1:1".to_string(), "probe-token".to_string());
    (HoloControlBridge::new(client, "holo", tx), rx)
}

#[tokio::main]
async fn main() {
    let (bridge, _rx) = unreachable_bridge();

    let idle = bridge.has_a_turn_streaming();
    println!("no turn running -> has_a_turn_streaming={idle} (a tier switch here is safe)");

    let mut during = false;
    tokio::join!(
        bridge.handle(ControlMessage::Prompt {
            request_id: "probe-turn".to_string(),
            text: "a turn that is mid-flight".to_string(),
            context_id: None,
        }),
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            during = bridge.has_a_turn_streaming();
            println!(
                "turn in flight -> has_a_turn_streaming={during} \
                 (force_tier must refuse here: switch_to kills and respawns holo serve)"
            );
        }
    );

    let after = bridge.has_a_turn_streaming();
    println!("turn finished -> has_a_turn_streaming={after} (escalation becomes safe again)");

    assert!(!idle, "an idle bridge must not claim a turn is streaming");
    assert!(
        during,
        "REGRESSION: an in-flight turn was invisible to force_tier's guard, so a tier switch \
         would restart holo serve underneath it -- the exact SSE-stream-errored failure"
    );
    assert!(
        !after,
        "REGRESSION: the turn-streaming flag never clears, which would permanently disable tier escalation"
    );

    println!("VERDICT: OK -- the guard sees a live turn exactly while one is in flight");
}
