//! Live witness for `permissions::secure_input_active()` -- confirms the raw
//! `IsSecureEventInputEnabled()` FFI binding actually links and returns a
//! sane result on real hardware, not just compiles. Read-only, no side
//! effects: safe to run at any time regardless of what's focused.
//!
//! Run with `cargo run --example secure_input_probe -p holoiroh-daemon`.

fn main() {
    let active = holoiroh_daemon::permissions::secure_input_active();
    println!("secure_input_active() = {active}");
    println!(
        "secure_input_probe: OK -- FFI call completed without crashing, returned a plain bool \
         ({active}). Expected false right now (a normal terminal has focus, not a password field)."
    );
}
