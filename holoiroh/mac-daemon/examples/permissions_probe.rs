//! Manual, run-by-hand probe: exercises the real `permissions::PreflightResult` /
//! `permissions::MissingPermission` construction and instruction text, printing real output.
//! Witnesses the pure data-shape logic that used to live in `permissions.rs`'s
//! `#[cfg(test)] mod tests` (removed per this repo's no-unit-tests rule).
//!
//! The actual macOS TCC queries (`screen_recording_granted`/`accessibility_granted`) are
//! separate, real, and already witnessed live elsewhere (via `cargo run --bin holoiroh-daemon`
//! on real macOS hardware) -- this probe only covers the pure `PreflightResult`/
//! `MissingPermission` logic named in `permissions.rs`'s own removed test module, as the task
//! that removed those tests scoped it.
//!
//! Run with `cargo run --example permissions_probe`.

use holoiroh_daemon::permissions::{MissingPermission, PreflightResult};

fn main() {
    println!("=== MissingPermission::instruction() names the exact Settings path ===");
    let sr = MissingPermission::ScreenRecording.instruction();
    let ax = MissingPermission::Accessibility.instruction();
    println!("ScreenRecording.instruction() contains \"Screen Recording\": {}", sr.contains("Screen Recording"));
    println!("Accessibility.instruction() contains \"Accessibility\": {}", ax.contains("Accessibility"));
    assert!(sr.contains("Screen Recording"));
    assert!(ax.contains("Accessibility"));

    println!();
    println!("=== PreflightResult::is_ok() when empty ===");
    let result = PreflightResult { missing: vec![] };
    println!("PreflightResult{{missing: []}}.is_ok() -> {}", result.is_ok());
    assert!(result.is_ok());

    println!();
    println!("=== PreflightResult::is_ok() when populated ===");
    let result = PreflightResult {
        missing: vec![MissingPermission::ScreenRecording],
    };
    println!(
        "PreflightResult{{missing: [ScreenRecording]}}.is_ok() -> {}",
        result.is_ok()
    );
    assert!(!result.is_ok());

    println!();
    println!("=== PreflightResult with both missing reports both (and report() to stderr) ===");
    let result = PreflightResult {
        missing: vec![
            MissingPermission::ScreenRecording,
            MissingPermission::Accessibility,
        ],
    };
    println!("missing.len() -> {}", result.missing.len());
    assert_eq!(result.missing.len(), 2);
    println!("calling result.report() now -- real stderr output follows:");
    result.report();

    println!();
    println!("permissions_probe: OK -- all PreflightResult/MissingPermission cases witnessed via real execution");
}
