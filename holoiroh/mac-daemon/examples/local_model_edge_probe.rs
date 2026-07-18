//! Adversarial edge-case probe for [`holoiroh_daemon::local_model::LocalModelConfig`]: exercises
//! degenerate/boundary/empty env inputs and the port-collision case by **real execution** (sets
//! real process env vars, calls the real `from_env()`, inspects the real built command), never a
//! test framework and never spawning `llama-server`.
//!
//! Run with `cargo run --example local_model_edge_probe`.

use holoiroh_daemon::local_model::{
    DEFAULT_LLAMA_SERVER_BIN, DEFAULT_LOCAL_MODEL_PORT, DEFAULT_MODEL_HF_REPO, LocalModelConfig,
};

struct Checks {
    failed: usize,
}
impl Checks {
    fn new() -> Self {
        Self { failed: 0 }
    }
    fn check(&mut self, label: &str, cond: bool) {
        println!("  [{}] {label}", if cond { "PASS" } else { "FAIL" });
        if !cond {
            self.failed += 1;
        }
    }
}

/// Clear every env var this probe touches, so each case starts from a known baseline regardless of
/// what the launching shell exported.
fn clear_env() {
    for k in [
        "HOLOIROH_LLAMA_BIN",
        "HOLOIROH_LOCAL_MODEL_HF_REPO",
        "HOLOIROH_LOCAL_MODEL_PORT",
    ] {
        unsafe {
            std::env::remove_var(k);
        }
    }
}

fn main() {
    // NOTE: std::env::set_var/remove_var are `unsafe` in edition 2024 (process-global mutation).
    // This probe is single-threaded and sets/reads env serially, so the safety requirement (no
    // concurrent env access from another thread) holds.
    let mut checks = Checks::new();
    println!("=== local_model config edge-case probe (no model spawn) ===\n");

    // --- Case 1: no env set -> all defaults ---
    clear_env();
    let cfg = LocalModelConfig::from_env();
    println!("Case 1: empty environment -> defaults");
    checks.check(
        "defaults to the Homebrew llama-server bin name",
        cfg.llama_server_bin == DEFAULT_LLAMA_SERVER_BIN,
    );
    checks.check(
        "defaults to the Holo3.1 Q4 HF repo",
        cfg.model_hf_repo == DEFAULT_MODEL_HF_REPO,
    );
    checks.check(
        "defaults to port 8080",
        cfg.port == DEFAULT_LOCAL_MODEL_PORT,
    );
    println!();

    // --- Case 2: blank/whitespace env vars are treated as unset (not empty overrides) ---
    clear_env();
    unsafe {
        std::env::set_var("HOLOIROH_LLAMA_BIN", "   ");
        std::env::set_var("HOLOIROH_LOCAL_MODEL_HF_REPO", "");
        std::env::set_var("HOLOIROH_LOCAL_MODEL_PORT", "  ");
    }
    let cfg = LocalModelConfig::from_env();
    println!("Case 2: blank/whitespace env vars -> ignored, defaults kept");
    checks.check(
        "blank bin ignored (not an empty program name that would fail to spawn)",
        cfg.llama_server_bin == DEFAULT_LLAMA_SERVER_BIN,
    );
    checks.check(
        "empty repo ignored (not an empty -hf arg)",
        cfg.model_hf_repo == DEFAULT_MODEL_HF_REPO,
    );
    checks.check(
        "whitespace port ignored (not a parse error, keeps default)",
        cfg.port == DEFAULT_LOCAL_MODEL_PORT,
    );
    println!();

    // --- Case 3: non-numeric / zero / out-of-range port -> keeps default (never binds port 0) ---
    for bad in ["not-a-number", "0", "70000", "-1", "8080.5"] {
        clear_env();
        unsafe {
            std::env::set_var("HOLOIROH_LOCAL_MODEL_PORT", bad);
        }
        let cfg = LocalModelConfig::from_env();
        checks.check(
            &format!("bad port {bad:?} -> falls back to default {DEFAULT_LOCAL_MODEL_PORT} (never 0/invalid)"),
            cfg.port == DEFAULT_LOCAL_MODEL_PORT,
        );
    }
    println!();

    // --- Case 4: a valid custom port is honored, and reflected in args + base_url + health_url ---
    clear_env();
    unsafe {
        std::env::set_var("HOLOIROH_LOCAL_MODEL_PORT", "9001");
    }
    let cfg = LocalModelConfig::from_env();
    println!("Case 4: valid custom port 9001 -> honored everywhere");
    checks.check("port parsed as 9001", cfg.port == 9001);
    checks.check(
        "base_url reflects the custom port",
        cfg.base_url() == "http://127.0.0.1:9001/v1",
    );
    checks.check(
        "health_url reflects the custom port",
        cfg.health_url() == "http://127.0.0.1:9001/health",
    );
    checks.check(
        "command args carry --port 9001",
        cfg.command_args().windows(2).any(|w| w[0] == "--port" && w[1] == "9001"),
    );
    checks.check(
        "command args still bind loopback only after a custom port",
        cfg.command_args().windows(2).any(|w| w[0] == "--host" && w[1] == "127.0.0.1"),
    );
    println!();

    // --- Case 5: boundary port values (1 and 65535) parse and never fall back ---
    for p in ["1", "65535"] {
        clear_env();
        unsafe {
            std::env::set_var("HOLOIROH_LOCAL_MODEL_PORT", p);
        }
        let cfg = LocalModelConfig::from_env();
        checks.check(
            &format!("boundary port {p} is honored (not clamped to default)"),
            cfg.port.to_string() == p,
        );
    }
    println!();

    // --- Case 6: a custom bin + repo (self-hosted mirror) flow through to the built command ---
    clear_env();
    unsafe {
        std::env::set_var("HOLOIROH_LLAMA_BIN", "/opt/custom/llama-server");
        std::env::set_var("HOLOIROH_LOCAL_MODEL_HF_REPO", "Some/Other-Model:Q5_K_M");
    }
    let cfg = LocalModelConfig::from_env();
    println!("Case 6: custom bin + repo overrides honored");
    let cmd = cfg.command();
    let prog = cmd.as_std().get_program().to_string_lossy().into_owned();
    let args: Vec<String> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    checks.check(
        "custom bin path is the command program",
        prog == "/opt/custom/llama-server",
    );
    checks.check(
        "custom repo is the -hf value",
        args.windows(2).any(|w| w[0] == "-hf" && w[1] == "Some/Other-Model:Q5_K_M"),
    );
    println!();

    clear_env();
    println!("=== summary ===");
    if checks.failed == 0 {
        println!("All edge-case checks PASSED (empty/blank env, bad/zero/out-of-range/boundary ports, custom overrides).");
    } else {
        eprintln!("{} edge-case check(s) FAILED.", checks.failed);
        std::process::exit(1);
    }
}
