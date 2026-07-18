//! Manual, run-by-hand probe for the **local (Aro Private) inference path**: builds the exact
//! subprocess commands the daemon would spawn -- the local `llama-server` and the `holo serve`
//! that points at it -- and prints their program, argv, and the env vars they set, then checks
//! (by real runtime assertions, printed, not a test framework) that the wiring is correct.
//!
//! **It never spawns the model.** The Holo3.1 Q4 GGUF is ~21 GB and takes minutes plus large RAM
//! to load; re-running that on every build would be wasteful. The real end-to-end latency of
//! actually serving it locally is measured separately and honestly in `holoiroh/BENCHMARKS.md`
//! (a real 8.3 s/step at 720p on this Apple M3 Pro / 36 GB Mac), not re-derived here. What this
//! probe proves is the piece that a benchmark cannot: that the daemon constructs the right
//! command and points `holo serve` at the local endpoint via the right env var
//! (`HAI_AGENT_RUNTIME_BASE_URL`, not `HAI_BASE_URL`) so inference stays on-box (PRD P0-11,
//! no cloud path).
//!
//! Optionally, pass `--bind-standin` to have the probe bind a trivial TCP listener on the local
//! model port for a moment, confirming the wiring targets a real, bindable loopback socket
//! without involving `llama-server` at all.
//!
//! Run with `cargo run --example local_model_probe` (or `... -- --bind-standin`).

use std::collections::BTreeMap;

use holoiroh_daemon::holo_bridge::process::HoloServeProcess;
use holoiroh_daemon::local_model::{
    DEFAULT_LOCAL_MODEL_PORT, DEFAULT_MODEL_HF_REPO, LOOPBACK_HOST, LocalModelConfig,
    RUNTIME_BASE_URL_ENV,
};

/// Extract `(program, args)` from a `tokio::process::Command` by borrowing the underlying
/// `std::process::Command`. `get_program`/`get_args` inspect the built command **without running
/// it** -- exactly the "construct and print, don't spawn" contract this probe exists to honor.
fn program_and_args(cmd: &tokio::process::Command) -> (String, Vec<String>) {
    let std_cmd = cmd.as_std();
    let program = std_cmd.get_program().to_string_lossy().into_owned();
    let args = std_cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    (program, args)
}

/// Extract the env vars this command *sets* (its overrides on top of the inherited environment),
/// as a sorted map. A `None` value means the command explicitly *removes* that var from the
/// child's environment (e.g. `env_remove("HAI_API_KEY")`), which we surface as `<removed>`.
fn env_overrides(cmd: &tokio::process::Command) -> BTreeMap<String, String> {
    let std_cmd = cmd.as_std();
    let mut out = BTreeMap::new();
    for (k, v) in std_cmd.get_envs() {
        let key = k.to_string_lossy().into_owned();
        let val = match v {
            Some(v) => v.to_string_lossy().into_owned(),
            None => "<removed>".to_string(),
        };
        out.insert(key, val);
    }
    out
}

fn print_command(title: &str, cmd: &tokio::process::Command) {
    let (program, args) = program_and_args(cmd);
    println!("--- {title} ---");
    println!("  program: {program}");
    println!("  argv   : {program} {}", args.join(" "));
    let envs = env_overrides(cmd);
    if envs.is_empty() {
        println!("  env    : (none set; inherits parent environment)");
    } else {
        println!("  env    :");
        for (k, v) in &envs {
            // Redact the bearer token's value -- its presence is what matters, not the secret.
            let shown = if k == "HOLO_AUTH_TOKEN" && v != "<removed>" {
                format!("<{}-char token>", v.len())
            } else {
                v.clone()
            };
            println!("    {k} = {shown}");
        }
    }
    println!();
}

/// A tiny assertion helper that prints PASS/FAIL and tracks failures, so the probe reports every
/// check's result (not just the first failure) and exits non-zero if any failed -- all via real
/// execution, with no test framework, no `#[cfg(test)]`, no assertion crate.
struct Checks {
    failed: usize,
}

impl Checks {
    fn new() -> Self {
        Self { failed: 0 }
    }

    fn check(&mut self, label: &str, cond: bool) {
        if cond {
            println!("  [PASS] {label}");
        } else {
            println!("  [FAIL] {label}");
            self.failed += 1;
        }
    }
}

fn main() {
    let bind_standin = std::env::args().any(|a| a == "--bind-standin");

    println!("=== holoiroh local (Aro Private) inference wiring probe ===\n");
    println!(
        "NOTE: This builds and inspects the subprocess commands ONLY. It does not spawn\n\
         llama-server -- the {DEFAULT_MODEL_HF_REPO} GGUF is ~21 GB and takes minutes to load.\n\
         A real live model-serving latency run (8.3 s/step @ 720p on this Mac) is documented\n\
         separately in holoiroh/BENCHMARKS.md and is NOT re-run here.\n"
    );

    let mut checks = Checks::new();

    // ---------------------------------------------------------------------------------------
    // 1. The local llama-server command.
    // ---------------------------------------------------------------------------------------
    let config = LocalModelConfig::default();
    let llama_cmd = config.command();
    print_command("llama-server command (local inference server)", &llama_cmd);

    let (llama_prog, llama_args) = program_and_args(&llama_cmd);
    let base_url = config.base_url();
    println!("  derived base_url : {base_url}");
    println!("  derived health   : {}\n", config.health_url());

    println!("  Checks for the llama-server command:");
    checks.check(
        "program is llama-server",
        llama_prog.ends_with("llama-server"),
    );
    checks.check(
        &format!("args carry `-hf {DEFAULT_MODEL_HF_REPO}`"),
        window_has(&llama_args, &["-hf", DEFAULT_MODEL_HF_REPO]),
    );
    checks.check(
        "args bind loopback only (`--host 127.0.0.1`)",
        window_has(&llama_args, &["--host", LOOPBACK_HOST]),
    );
    checks.check(
        "args never bind 0.0.0.0 (no external exposure)",
        !llama_args.iter().any(|a| a == "0.0.0.0"),
    );
    checks.check(
        &format!("args carry `--port {DEFAULT_LOCAL_MODEL_PORT}`"),
        window_has(&llama_args, &["--port", &DEFAULT_LOCAL_MODEL_PORT.to_string()]),
    );
    checks.check(
        "does NOT pass --no-mmproj (vision projector must load for screenshot inference)",
        !llama_args.iter().any(|a| a == "--no-mmproj"),
    );
    checks.check(
        "base_url is the OpenAI-compatible loopback endpoint (http://127.0.0.1:<port>/v1)",
        base_url == format!("http://{LOOPBACK_HOST}:{DEFAULT_LOCAL_MODEL_PORT}/v1"),
    );
    println!();

    // ---------------------------------------------------------------------------------------
    // 2. The `holo serve` command, pointed at the local server.
    // ---------------------------------------------------------------------------------------
    // A representative holo-serve port distinct from the model port (main.rs defaults 8765).
    let holo_port: u16 = 8765;
    let fake_token = "fake-token-for-inspection-only";
    let holo_cmd =
        HoloServeProcess::build_command("holo", holo_port, Some(base_url.as_str()), fake_token);
    print_command(
        "holo serve command (pointed at the LOCAL model)",
        &holo_cmd,
    );

    let (_holo_prog, holo_args) = program_and_args(&holo_cmd);
    let holo_env = env_overrides(&holo_cmd);

    println!("  Checks for the holo serve command (LOCAL mode):");
    checks.check(
        "args carry `serve`",
        holo_args.iter().any(|a| a == "serve"),
    );
    checks.check(
        &format!("args carry `--port {holo_port}`"),
        window_has(&holo_args, &["--port", &holo_port.to_string()]),
    );
    checks.check(
        "args carry `--base-url <local /v1 url>`",
        window_has(&holo_args, &["--base-url", &base_url]),
    );
    checks.check(
        &format!("env sets {RUNTIME_BASE_URL_ENV} to the local base_url (the var that redirects INFERENCE)"),
        holo_env.get(RUNTIME_BASE_URL_ENV).map(String::as_str) == Some(base_url.as_str()),
    );
    checks.check(
        "env does NOT set HAI_BASE_URL (that only overrides the cloud entitlement gateway, not inference)",
        !holo_env.contains_key("HAI_BASE_URL"),
    );
    checks.check(
        "env explicitly REMOVES HAI_API_KEY so the hosted key can't leak to the local endpoint (no-cloud, P0-11)",
        holo_env.get("HAI_API_KEY").map(String::as_str) == Some("<removed>"),
    );
    checks.check(
        "env sets HOLO_AUTH_TOKEN (bearer for holo serve's own A2A surface)",
        holo_env.contains_key("HOLO_AUTH_TOKEN"),
    );
    checks.check(
        "local model port differs from holo serve port (two distinct listeners)",
        DEFAULT_LOCAL_MODEL_PORT != holo_port,
    );
    println!();

    // ---------------------------------------------------------------------------------------
    // 3. Contrast: the CLOUD/default holo serve command (no local base_url) still works and does
    //    NOT drop HAI_API_KEY -- proving the local branch is what enforces no-cloud, and that the
    //    hosted path is a real, separate shape (not accidentally always-on).
    // ---------------------------------------------------------------------------------------
    let cloud_cmd = HoloServeProcess::build_command("holo", holo_port, None, fake_token);
    print_command(
        "holo serve command (NO local base_url -- hosted/default shape, for contrast)",
        &cloud_cmd,
    );
    let cloud_env = env_overrides(&cloud_cmd);
    let (_c, cloud_args) = program_and_args(&cloud_cmd);
    println!("  Checks for the hosted/default holo serve command:");
    checks.check(
        "no --base-url arg when no local URL is configured",
        !cloud_args.iter().any(|a| a == "--base-url"),
    );
    checks.check(
        &format!("does NOT set {RUNTIME_BASE_URL_ENV} in hosted mode"),
        !cloud_env.contains_key(RUNTIME_BASE_URL_ENV),
    );
    checks.check(
        "does NOT force-remove HAI_API_KEY in hosted mode (inherits it for the hosted gateway)",
        cloud_env.get("HAI_API_KEY").map(String::as_str) != Some("<removed>"),
    );
    println!();

    // ---------------------------------------------------------------------------------------
    // 4. Optional stand-in: bind a trivial listener on the local model port to confirm the wiring
    //    targets a real, bindable loopback socket -- WITHOUT llama-server or the model.
    // ---------------------------------------------------------------------------------------
    if bind_standin {
        println!("=== --bind-standin: binding a trivial listener on the local model port ===");
        let addr = format!("{LOOPBACK_HOST}:{DEFAULT_LOCAL_MODEL_PORT}");
        match std::net::TcpListener::bind(&addr) {
            Ok(listener) => {
                let local = listener
                    .local_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| addr.clone());
                checks.check(
                    &format!("bound a stand-in TCP listener on {local} (loopback socket is real & bindable)"),
                    true,
                );
                checks.check(
                    "stand-in bound to loopback (127.0.0.1), never a routable address",
                    listener
                        .local_addr()
                        .map(|a| a.ip().is_loopback())
                        .unwrap_or(false),
                );
                // Drop immediately -- we only proved the port is a real bindable loopback socket.
                drop(listener);
            }
            Err(err) => {
                println!(
                    "  [SKIP] could not bind {addr} ({err}); likely something is already on that\n\
                     port (e.g. a real llama-server or a previous run). This is not a wiring\n\
                     failure -- the command construction above is what this probe verifies."
                );
            }
        }
        println!();
    } else {
        println!(
            "(pass `--bind-standin` to also bind a trivial listener on the local model port and\n\
             confirm it is a real, loopback-only bindable socket -- still without llama-server.)\n"
        );
    }

    // ---------------------------------------------------------------------------------------
    // Summary.
    // ---------------------------------------------------------------------------------------
    println!("=== summary ===");
    if checks.failed == 0 {
        println!(
            "All checks PASSED. The daemon builds the correct llama-server command and points\n\
             holo serve at the LOCAL inference endpoint via {RUNTIME_BASE_URL_ENV} (+ --base-url),\n\
             with HAI_API_KEY removed on the local path -- inference stays on 127.0.0.1, no cloud.\n\
             A full live model-serving run is heavy and separately benchmarked (BENCHMARKS.md),\n\
             not performed by this probe."
        );
    } else {
        eprintln!("{} check(s) FAILED -- see [FAIL] lines above.", checks.failed);
        std::process::exit(1);
    }
}

/// True if `args` contains `needle` as a contiguous subsequence (order-preserving). Used to assert
/// flag/value pairs like `["--host", "127.0.0.1"]` appear adjacently in the built argv.
fn window_has(args: &[String], needle: &[&str]) -> bool {
    if needle.is_empty() || needle.len() > args.len() {
        return false;
    }
    args.windows(needle.len())
        .any(|w| w.iter().zip(needle).all(|(a, n)| a.as_str() == *n))
}
