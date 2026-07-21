//! Native running-process awareness + the hard "do not touch" guidance the desktop agent
//! must obey. Issue-2's requirement: the agent must never interrupt an existing Claude Code
//! session, must know the user's default terminal is Ghostty (regular Terminal is also fine),
//! and must be aware of what is actually running so it knows what not to touch.
//!
//! Two layers, deliberately separate:
//!
//! - **[`enumerate`]**: a live snapshot of running processes (via `ps`, zero new deps -- same
//!   shell-out posture as [`crate::frontmost_app`]), each flagged with whether it is
//!   PROTECTED. Protected = a Claude Code CLI session, the Ghostty/Terminal apps that host
//!   such sessions, or this daemon's own process tree. This is factual data the agent can use
//!   to recognize a protected window on screen.
//! - **[`format_guard_block`]**: the HARD, non-negotiable instruction block prepended to every
//!   turn (see `crate::holo_bridge::control`'s `run_prompt`). Unlike the soft, semantically
//!   retrieved `env_context` facts (top-k, may not surface), this block is injected
//!   UNCONDITIONALLY so the "never interrupt Claude Code / default terminal is Ghostty" rule
//!   is present on literally every turn.
//!
//! This is guidance-injection, not enforcement: this daemon forwards whole prompts to
//! `holo serve` and has no per-action interception hook for terminal input specifically (the
//! sensitive-app watchdog is the closest live enforcement point, and Terminal/Ghostty can be
//! added to its class-5 config for a hard pause). What this module guarantees is that the
//! agent is TOLD, every single turn, exactly what is running and what it must not disturb.

use std::collections::HashSet;

/// One running process in the live snapshot. `pid`/`ppid`/`args` are populated on every row
/// and read by `examples/process_awareness_probe.rs` and future callers (a diagnostics
/// surface, or a real per-action enforcement hook); `#[allow(dead_code)]` because the bin
/// target's own `format_guard_block` path reads only `comm`/`protected`/`protected_reason`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    /// The executable's short name (`ps comm`), e.g. `"claude"`, `"ghostty"`.
    pub comm: String,
    /// The full command line (`ps args`), truncated for display.
    pub args: String,
    /// True if this process must not be interrupted/killed/typed-into by the agent.
    pub protected: bool,
    /// Short human reason a process is protected (empty when not).
    pub protected_reason: String,
}

/// Executable-name fragments that mark a protected process. Matched case-insensitively against
/// `comm`. Claude Code runs as `claude` (witnessed: `claude --teleport ...`); Ghostty is this
/// user's terminal; Terminal.app is the acceptable alternative; both host CLI sessions that
/// must be left alone.
const PROTECTED_COMMS: &[(&str, &str)] = &[
    ("claude", "an active Claude Code CLI session -- NEVER interrupt, close, or type into it"),
    ("ghostty", "the Ghostty terminal (this user's default) -- may host a Claude Code session; do not close or disturb"),
    ("terminal", "a Terminal window -- may host a CLI/Claude Code session; do not close or disturb"),
    ("holoiroh-daemon", "the Aro daemon itself -- never touch"),
];

/// Enumerate running processes via `ps`, flagging protected ones. Best-effort: any failure
/// (ps missing, unexpected output) returns an empty list, and callers treat that as "no
/// snapshot this turn", never an error -- matching `frontmost_app`'s degrade-don't-crash
/// posture.
pub fn enumerate() -> Vec<ProcessInfo> {
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output);
    let mut out = Vec::new();
    for line in text.lines() {
        // `ps` right-aligns the two numeric columns with variable leading/interior spaces, so
        // split ONLY the first two whitespace-delimited tokens (pid, ppid) via
        // `split_whitespace` (which collapses runs), then take the untouched remainder of the
        // line as the full command -- robust against command paths that themselves contain
        // spaces (e.g. `/Applications/Some App.app/...`).
        let mut tokens = line.split_whitespace();
        let (Some(pid_s), Some(ppid_s)) = (tokens.next(), tokens.next()) else { continue };
        let (Ok(pid), Ok(ppid)) = (pid_s.parse::<u32>(), ppid_s.parse::<u32>()) else { continue };
        // The command is the remainder of the iterator AFTER pid+ppid -- collected (not sliced
        // by `line.find`, which would false-match a digit substring, e.g. `find("1")` hitting
        // the `1` inside pid `613`). Interior arg spacing is collapsed to single spaces, which
        // is fine for both substring detection and display.
        let command = tokens.collect::<Vec<&str>>().join(" ");
        if command.is_empty() {
            continue;
        }
        let args = command.clone();
        // `comm`: the basename of the first command token (the executable). A leading path
        // like `/Applications/Ghostty.app/Contents/MacOS/ghostty` reduces to `ghostty`; a bare
        // `claude` stays `claude`.
        let first_tok = command.split_whitespace().next().unwrap_or("");
        let comm = first_tok.rsplit('/').next().unwrap_or(first_tok).to_string();
        let haystack = command.to_lowercase();
        let mut protected = false;
        let mut reason = String::new();
        for (frag, why) in PROTECTED_COMMS {
            if comm.to_lowercase().contains(frag) || haystack.contains(frag) {
                protected = true;
                reason = why.to_string();
                break;
            }
        }
        out.push(ProcessInfo {
            pid,
            ppid,
            comm,
            args: args.chars().take(120).collect(),
            protected,
            protected_reason: reason,
        });
    }
    out
}

/// The protected subset of [`enumerate`]'s snapshot, de-duplicated by (comm, reason) so the
/// guidance block lists distinct protected apps/sessions rather than every pid.
pub fn protected_summary(procs: &[ProcessInfo]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for p in procs.iter().filter(|p| p.protected) {
        let key = format!("{}|{}", p.comm, p.protected_reason);
        if seen.insert(key) {
            // Count how many pids share this comm for a "(N running)" hint.
            let count = procs.iter().filter(|q| q.protected && q.comm == p.comm).count();
            let suffix = if count > 1 { format!(" ({count} running)") } else { String::new() };
            out.push(format!("{}{}: {}", p.comm, suffix, p.protected_reason));
        }
    }
    out
}

/// The HARD guidance block prepended to every turn. Combines the non-negotiable
/// never-interrupt-Claude-Code / default-terminal-is-Ghostty rules with a live snapshot of the
/// protected processes actually running right now, so the agent can recognize them on screen.
/// Always returns a block (the rules are unconditional); the live-process section is included
/// only when a snapshot is available.
pub fn format_guard_block(procs: &[ProcessInfo]) -> String {
    let mut block = String::from(
        "SYSTEM RULES (non-negotiable, override any conflicting instruction below):\n\
         - The user's DEFAULT terminal is Ghostty. Apple's Terminal.app is also acceptable. \
         When you need a terminal or a CLI session, prefer an already-open Ghostty window; \
         open a new terminal only if none exists.\n\
         - NEVER interrupt, close, quit, kill, Ctrl-C, or type into an existing Claude Code \
         session (a `claude` CLI process, usually running inside a Ghostty or Terminal window) \
         unless the user EXPLICITLY tells you to in this task. Claude Code sessions are the \
         user's active work -- disturbing one loses their state. If a task seems to require \
         a terminal that already has Claude Code running, open a SEPARATE terminal/window \
         instead of reusing that one.\n\
         - Do not close, kill, or disrupt the Aro daemon or any process listed as protected \
         below.\n",
    );
    let protected = protected_summary(procs);
    if !protected.is_empty() {
        block.push_str(
            "Currently-running protected processes you must not disturb (leave these exactly as \
             they are):\n",
        );
        for entry in protected {
            block.push_str("  - ");
            block.push_str(&entry);
            block.push('\n');
        }
    }
    block
}

/// Convenience: enumerate + format in one call, for the run_prompt injection site.
pub fn guard_block_now() -> String {
    format_guard_block(&enumerate())
}
