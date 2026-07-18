//! Library surface for `holoiroh-daemon`, re-exporting the modules that
//! need to be reachable from outside the binary crate: integration-style
//! examples/tests that dial the daemon's control channel as a real `iroh`
//! peer (see `examples/control_probe.rs`), and any future test harness.
//!
//! `main.rs` remains the actual daemon entrypoint and uses these same
//! modules via its own `mod` declarations tied to this lib target (Rust
//! resolves `mod control_channel;` in `main.rs` to `src/control_channel.rs`
//! either way, so declaring the modules once here and having `main.rs`
//! pull them in via `use holoiroh_daemon::...` would work equally, but
//! `main.rs` keeps its own `mod` statements so `cargo build --bin
//! holoiroh-daemon` alone, without the lib target, still exercises the
//! exact same source files -- both targets compile the same `.rs` files,
//! just under two different crate roots).

pub mod allowlist;
pub mod audit_log;
pub mod auth;
pub mod capture;
pub mod control_channel;
pub mod holo_bridge;
pub mod limits;
pub mod permissions;
pub mod sensitive_categories;
pub mod task_state;
