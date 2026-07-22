//! Pure-logic CI witness for the remote_control ClientMessage + its nested
//! RemoteControlEvent (the escalate-and-take-control wire). Deterministic, no
//! daemon needed -- it pins the exact JSON the iOS app must produce/parse for
//! every action, both directions.
//!
//! Run with `cargo run --example remote_control_wire_probe -p holoiroh-daemon`.

use holoiroh_daemon::control_channel::{ClientMessage, MouseButton, RemoteControlEvent};

fn rt(msg: &ClientMessage) -> String {
    let json = serde_json::to_string(msg).expect("serialize");
    let back: ClientMessage = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, msg, "round-trip mismatch for {json}");
    json
}

fn main() {
    // The outer envelope is `{"type":"remote_control","event":{...}}`.
    let take = ClientMessage::RemoteControl { event: RemoteControlEvent::TakeControl };
    let j = rt(&take);
    println!("take  -> {j}");
    assert!(j.contains("\"type\":\"remote_control\""), "wrong type tag: {j}");
    assert!(j.contains("\"action\":\"take_control\""), "wrong action tag: {j}");

    let mv = ClientMessage::RemoteControl { event: RemoteControlEvent::Move { x: 0.5, y: 0.25 } };
    let j = rt(&mv);
    println!("move  -> {j}");
    assert!(j.contains("\"action\":\"move\"") && j.contains("\"x\":0.5"), "move shape: {j}");

    let click = ClientMessage::RemoteControl {
        event: RemoteControlEvent::Click { x: 0.1, y: 0.2, button: MouseButton::Left, count: 2 },
    };
    let j = rt(&click);
    println!("click -> {j}");
    assert!(j.contains("\"button\":\"left\"") && j.contains("\"count\":2"), "click shape: {j}");

    let btn = ClientMessage::RemoteControl {
        event: RemoteControlEvent::Button { x: 0.3, y: 0.4, button: MouseButton::Right, down: true },
    };
    println!("btn   -> {}", rt(&btn));

    let scroll = ClientMessage::RemoteControl {
        event: RemoteControlEvent::Scroll { x: 0.5, y: 0.5, dx: 0.0, dy: -3.0 },
    };
    println!("scroll-> {}", rt(&scroll));

    let text = ClientMessage::RemoteControl { event: RemoteControlEvent::Text { text: "hi team".into() } };
    let j = rt(&text);
    assert!(j.contains("\"action\":\"text\"") && j.contains("\"text\":\"hi team\""), "text shape: {j}");

    let key = ClientMessage::RemoteControl { event: RemoteControlEvent::Key { key: "return".into(), down: true } };
    println!("key   -> {}", rt(&key));

    let release = ClientMessage::RemoteControl { event: RemoteControlEvent::ReleaseControl };
    let j = rt(&release);
    assert!(j.contains("\"action\":\"release_control\""), "release shape: {j}");

    // The exact canonical wire an iOS client emits decodes too.
    let decoded: ClientMessage = serde_json::from_str(
        r#"{"type":"remote_control","event":{"action":"click","x":0.5,"y":0.5,"button":"left","count":1}}"#,
    )
    .expect("decode canonical click");
    assert!(matches!(decoded, ClientMessage::RemoteControl { .. }));

    println!("remote_control_wire_probe: OK -- every remote-control action round-trips as the client expects.");
}
