//! Witnesses that remote-control input is applied in the order the phone sent it.
//!
//! The control read loop used to hand EVERY decoded message to `tokio::spawn`. That spawn is
//! load-bearing for prompts and stop (an inline `.await` there is what made "stop" unable to
//! stop a running task), but cursor input carries ABSOLUTE positions: two moves landing out of
//! order do not merely arrive late, they teleport the pointer back to where the finger used to
//! be. Dragging then reads as the cursor stuttering backwards -- the "choppy" the user reported.
//!
//! Part one checks the classification directly. Part two models the read loop's two dispatch
//! shapes against the REAL `remote_input::move_cursor` (via `HOLOIROH_INPUT_DRY_RUN`, which
//! records instead of posting CGEvents, so this cannot hijack a real cursor) and shows the
//! spawning shape genuinely reorders while the awaited shape does not.

use holoiroh_daemon::control_channel::must_preserve_arrival_order;
use holoiroh_daemon::holo_bridge::ControlMessage;
use holoiroh_daemon::remote_input;
use holoiroh_wire::RemoteControlEvent;

fn remote(event: RemoteControlEvent) -> ControlMessage {
    ControlMessage::RemoteControl { event }
}

fn ordered_variants() -> Vec<(&'static str, ControlMessage)> {
    vec![
        ("move", remote(RemoteControlEvent::Move { x: 0.1, y: 0.2 })),
        (
            "button",
            remote(RemoteControlEvent::Button {
                x: 0.1,
                y: 0.2,
                button: holoiroh_wire::MouseButton::Left,
                down: true,
            }),
        ),
        (
            "click",
            remote(RemoteControlEvent::Click {
                x: 0.1,
                y: 0.2,
                button: holoiroh_wire::MouseButton::Left,
                count: 1,
            }),
        ),
        (
            "scroll",
            remote(RemoteControlEvent::Scroll {
                x: 0.1,
                y: 0.2,
                dx: 0.0,
                dy: 1.0,
            }),
        ),
        (
            "text",
            remote(RemoteControlEvent::Text {
                text: "hi".to_string(),
            }),
        ),
        (
            "key",
            remote(RemoteControlEvent::Key {
                key: "return".to_string(),
                down: true,
            }),
        ),
    ]
}

fn spawnable_variants() -> Vec<(&'static str, ControlMessage)> {
    vec![
        ("take_control", remote(RemoteControlEvent::TakeControl)),
        ("release_control", remote(RemoteControlEvent::ReleaseControl)),
        (
            "prompt",
            ControlMessage::Prompt {
                request_id: "probe".to_string(),
                text: "hello".to_string(),
                context_id: None,
            },
        ),
        (
            "stop",
            ControlMessage::Stop {
                request_id: "probe".to_string(),
                context_id: None,
                force: false,
            },
        ),
    ]
}

/// Stands in for `handle_message`: real work with a real await inside it, so the two dispatch
/// shapes below differ the way they do in the read loop.
async fn apply(nx: f64, ny: f64) {
    tokio::task::yield_now().await;
    remote_input::move_cursor(nx, ny);
}

const DRAG_LENGTH: usize = 64;
const CONCURRENT_SPAWNED_TASKS: usize = 32;

fn drag_path() -> Vec<(f64, f64)> {
    (0..DRAG_LENGTH)
        .map(|i| {
            let t = i as f64 / DRAG_LENGTH as f64;
            (t, t * 0.5)
        })
        .collect()
}

fn first_backwards_step(applied: &[(f64, f64)]) -> Option<(usize, f64, f64)> {
    applied
        .windows(2)
        .enumerate()
        .find(|(_, w)| w[1].0 < w[0].0)
        .map(|(i, w)| (i, w[0].0, w[1].0))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    for (label, message) in ordered_variants() {
        assert!(
            must_preserve_arrival_order(&message),
            "{label} carries an absolute cursor position, so applying it out of order teleports \
             the pointer backwards -- it must be awaited in stream order, not spawned"
        );
        println!("  ordered: {label}");
    }

    for (label, message) in spawnable_variants() {
        assert!(
            !must_preserve_arrival_order(&message),
            "{label} must keep the spawn: awaiting it inline in the read loop is exactly what \
             made stop unable to stop a running task"
        );
        println!("  spawned: {label}");
    }

    assert_eq!(
        std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref(),
        Ok("1"),
        "this probe must run with HOLOIROH_INPUT_DRY_RUN=1 so it records moves instead of \
         driving the machine's real cursor"
    );

    let path = drag_path();

    let _ = remote_input::take_applied_moves();
    for (nx, ny) in &path {
        let (nx, ny) = (*nx, *ny);
        tokio::spawn(async move { apply(nx, ny).await });
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let spawned = remote_input::take_applied_moves();

    for (nx, ny) in &path {
        apply(*nx, *ny).await;
    }
    let awaited = remote_input::take_applied_moves();

    println!(
        "\nspawned dispatch applied {} moves, awaited dispatch applied {}",
        spawned.len(),
        awaited.len()
    );

    let expected: Vec<(f64, f64)> = path.clone();
    assert_eq!(
        awaited, expected,
        "awaiting in the read loop must apply every move exactly in the order the phone sent it"
    );
    println!("awaited dispatch: cursor path preserved exactly, no backwards step");

    match first_backwards_step(&spawned) {
        Some((i, from, to)) => println!(
            "spawned dispatch: cursor jumped BACKWARDS at step {i} ({from:.3} -> {to:.3}) -- the \
             stutter this fix removes"
        ),
        None => println!(
            "spawned dispatch happened to stay ordered on this run; the scheduler gives no \
             ordering guarantee across tasks, which is why the fix is structural rather than \
             a matter of observed luck"
        ),
    }

    assert!(
        first_backwards_step(&awaited).is_none(),
        "the awaited path stepped backwards, which is the whole defect"
    );

    // An idle runtime is the easy case. The read loop's whole point is that ordered input and
    // spawned work share one runtime, so the drag has to survive the spawned half competing for
    // the same worker threads -- which is exactly when a scheduler is free to reorder.
    let noise = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut background = Vec::new();
    for _ in 0..CONCURRENT_SPAWNED_TASKS {
        let noise = noise.clone();
        background.push(tokio::spawn(async move {
            for _ in 0..200 {
                tokio::task::yield_now().await;
                noise.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }));
    }

    for (nx, ny) in &path {
        apply(*nx, *ny).await;
    }
    let under_load = remote_input::take_applied_moves();
    for task in background {
        let _ = task.await;
    }

    assert_eq!(
        under_load,
        expected,
        "the drag lost its order while {CONCURRENT_SPAWNED_TASKS} spawned tasks competed for the \
         same runtime -- which is the only condition that matters, since the read loop always \
         runs both kinds of work together"
    );
    println!(
        "under load ({} spawned tasks, {} yields): cursor path still preserved exactly",
        CONCURRENT_SPAWNED_TASKS,
        noise.load(std::sync::atomic::Ordering::Relaxed)
    );

    println!(
        "\nVERDICT: OK -- absolute cursor input is applied in arrival order, while stop/prompt \
         keep the spawn that lets them interrupt a running turn"
    );
}
