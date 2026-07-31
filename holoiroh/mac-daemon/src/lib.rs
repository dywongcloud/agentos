//! Library surface for `holoiroh-daemon`. This crate re-exports the modules
//! that must be reachable from outside the binary crate: integration-style
//! examples and tests that dial the daemon's control channel as a real
//! `iroh` peer (see `examples/control_probe.rs`), and any future test
//! harness.
//!
//! `main.rs` remains the actual daemon entrypoint. It uses these same
//! modules through its own `mod` declarations, tied to the binary crate
//! root. `main.rs` could instead pull the modules in through `use
//! holoiroh_daemon::...`, since Rust resolves `mod control_channel;` to
//! `src/control_channel.rs` either way. `main.rs` keeps its own `mod`
//! statements instead, so `cargo build --bin holoiroh-daemon` alone, without
//! the lib target, still compiles the exact same source files. Both targets
//! compile the same `.rs` files, just under two different crate roots.

pub mod agent_guidance;
pub mod allowlist;
pub mod audit_log;
pub mod auth;
pub mod auto_yield;
pub mod clarify;
pub mod remote_input;
pub mod user_activity;
pub mod capture;
pub mod control_channel;
pub mod duration;
pub mod executor;
pub mod frontmost_app;
pub mod holo_bridge;
pub mod limits;
pub mod local_model;
pub mod pairing_phrase;
pub mod permissions;
pub mod policy;
pub mod process_awareness;
pub mod registry;
pub mod sensitive_categories;
pub mod task_state;
pub mod router;
pub mod env_context;
pub mod task_fsm;
pub mod tinfoil_proxy;
pub mod privacy;
pub mod tinfoil_models;
pub mod tinfoil_documents;
pub mod tinfoil_vision;
pub mod tinfoil_audio;
pub mod tinfoil_planner;
pub mod tmux;
