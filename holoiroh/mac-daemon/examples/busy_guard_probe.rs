//! Proves the single-active-turn `busy` flag is released even when a turn dies
//! abnormally.
//!
//! Before `BusyGuard`, `busy` was cleared only by `drain_queue`'s normal return.
//! A panic (or task cancellation) anywhere inside a turn therefore left it stuck
//! `true` forever, silently converting the daemon into "queue every future
//! prompt, run none" -- the phone shows `queued, 0 ahead` for every task, with
//! nothing logging an error.
//!
//! This models that exact lifecycle over the same std::sync::Mutex<bool> and
//! guard type, and asserts recovery.
//!
//! Run: cargo run --release --example busy_guard_probe

use std::sync::Mutex;

/// Mirror of `holo_bridge::control::BusyGuard` (private to that module).
struct BusyGuard<'a> {
    busy: &'a Mutex<bool>,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        if *busy {
            println!("  [guard] turn ended dirty -- releasing busy");
            *busy = false;
        }
    }
}

fn run_turn(busy: &Mutex<bool>, panics: bool, guarded: bool) {
    {
        let mut b = busy.lock().unwrap_or_else(|e| e.into_inner());
        *b = true;
    }
    let _g = if guarded {
        Some(BusyGuard { busy })
    } else {
        None
    };
    if panics {
        panic!("simulated failure inside run_prompt");
    }
    // Normal completion: drain_queue's equivalent clears it.
    let mut b = busy.lock().unwrap_or_else(|e| e.into_inner());
    *b = false;
}

fn main() {
    println!("=== WITHOUT the guard (the original behaviour) ===");
    let busy = Mutex::new(false);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_turn(&busy, true, false)
    }));
    let stuck = *busy.lock().unwrap_or_else(|e| e.into_inner());
    println!("  busy after a panicking turn: {stuck}");
    assert!(stuck, "expected the unguarded version to wedge");
    println!("  -> every later prompt would queue forever\n");

    println!("=== WITH the guard (current behaviour) ===");
    let busy = Mutex::new(false);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_turn(&busy, true, true)));
    let after_panic = *busy.lock().unwrap_or_else(|e| e.into_inner());
    println!("  busy after a panicking turn: {after_panic}");

    // And a normal turn must still work, and must not be disturbed by the guard.
    run_turn(&busy, false, true);
    let after_ok = *busy.lock().unwrap_or_else(|e| e.into_inner());
    println!("  busy after a clean turn:     {after_ok}");

    println!("\nVERDICT:");
    if !after_panic && !after_ok {
        println!("  PASS: busy is released after a panicking turn, and the normal");
        println!("  path is unaffected. The daemon can still run tasks after a failure.");
    } else {
        println!("  FAIL: busy left stuck (panic={after_panic}, clean={after_ok})");
        std::process::exit(1);
    }
}
