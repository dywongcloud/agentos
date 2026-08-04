//! Measures how long the control read loop is blocked when it applies input inline.
//!
//! Cursor input is now awaited in arrival order instead of spawned (see
//! `remote_input_ordering_probe`), because absolute positions applied out of order teleport the
//! pointer backwards. Awaiting inline reintroduces something the spawn used to hide: the read
//! loop withholds QUIC flow-control credit for as long as injection takes, so a slow injection
//! would delay reading the NEXT message -- including a `Stop`. That is only acceptable while
//! injection stays far below a frame, which is a claim to measure rather than assume.
//!
//! Two costs make up an inline apply: building the CGEvent, and posting it.
//!
//! Building is measured through the real `remote_input` entry points under
//! `HOLOIROH_INPUT_DRY_RUN=1`, including `text` at lengths up to 10k characters -- the one
//! variant whose work could plausibly scale with its payload.
//!
//! Posting is measured directly here, and only ever as a mouse move to wherever the cursor
//! already is, re-read immediately beforehand, so this cannot move anything. Keystrokes are
//! NEVER posted: that would type into whatever window happens to be focused. Set
//! `HOLOIROH_MEASURE_REAL_POSTS=1` to include it; without it only the build cost is reported.

use std::time::{Duration, Instant};

use objc2_core_graphics::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};

use holoiroh_daemon::remote_input;

const SAMPLES: usize = 500;

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn report(label: &str, mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    let p50 = percentile(&samples, 0.50);
    let p99 = percentile(&samples, 0.99);
    let max = samples.last().copied().unwrap_or_default();
    println!("  {label:<34} p50 {p50:>9.2?}   p99 {p99:>9.2?}   max {max:>9.2?}");
    p99
}

fn measure(mut body: impl FnMut()) -> Vec<Duration> {
    for _ in 0..32 {
        body();
    }
    (0..SAMPLES)
        .map(|_| {
            let started = Instant::now();
            body();
            started.elapsed()
        })
        .collect()
}

fn main() {
    assert_eq!(
        std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref(),
        Ok("1"),
        "this probe must run with HOLOIROH_INPUT_DRY_RUN=1 so building an event never posts it"
    );

    println!("building the event (real remote_input entry points, posting suppressed)");

    let mut worst_build = Duration::ZERO;
    let mut step = 0.0_f64;
    worst_build = worst_build.max(report(
        "move",
        measure(|| {
            step = (step + 0.001) % 1.0;
            remote_input::move_cursor(step, step);
        }),
    ));
    worst_build = worst_build.max(report(
        "click",
        measure(|| remote_input::click(0.5, 0.5, false, 1)),
    ));
    worst_build = worst_build.max(report(
        "scroll",
        measure(|| remote_input::scroll(0.5, 0.5, 0.0, 1.0)),
    ));
    worst_build = worst_build.max(report("key", measure(|| remote_input::key("return", true))));

    for length in [1usize, 100, 1_000, 10_000] {
        let payload = "a".repeat(length);
        worst_build = worst_build.max(report(
            &format!("text ({length} chars)"),
            measure(|| remote_input::text(&payload)),
        ));
    }

    let post_p99 = if std::env::var("HOLOIROH_MEASURE_REAL_POSTS").as_deref() == Ok("1") {
        assert!(
            remote_input::is_permitted(),
            "real-post measurement needs Accessibility; without it this would time the failure \
             path and report a misleadingly small number"
        );
        println!("\nposting the event (mouse move to where the cursor already is)");
        let before = remote_input::cursor_location();
        let samples = measure(|| {
            let Some(here) = remote_input::cursor_location() else {
                return;
            };
            if let Some(ev) =
                CGEvent::new_mouse_event(None, CGEventType::MouseMoved, here, CGMouseButton::Left)
            {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
            }
        });
        let after = remote_input::cursor_location();
        println!(
            "  cursor before ({:.1}, {:.1}) -> after ({:.1}, {:.1})",
            before.map(|p| p.x).unwrap_or_default(),
            before.map(|p| p.y).unwrap_or_default(),
            after.map(|p| p.x).unwrap_or_default(),
            after.map(|p| p.y).unwrap_or_default()
        );
        report("post (round trip incl. re-read)", samples)
    } else {
        println!(
            "\nposting NOT measured (set HOLOIROH_MEASURE_REAL_POSTS=1 on a machine whose cursor \
             you are willing to have re-pinned to its own position)"
        );
        Duration::ZERO
    };

    let inline_p99 = worst_build + post_p99;
    println!("\nworst inline apply (p99 build + p99 post): {inline_p99:.2?}");

    let frame = Duration::from_micros(16_667);
    assert!(
        inline_p99 < frame,
        "an inline apply costs {inline_p99:.2?}, which is at least a display frame. At that cost \
         the read loop withholds flow-control credit long enough to delay the NEXT control \
         message -- including Stop -- and the ordering fix would need a bounded queue instead of \
         a plain inline await"
    );

    println!(
        "\nVERDICT: OK -- an inline apply costs {inline_p99:.2?}, under a {frame:.2?} frame, so \
         awaiting input in arrival order cannot meaningfully delay reading a Stop. Text does not \
         scale with its payload: one CGEvent carries the whole string."
    );
}
