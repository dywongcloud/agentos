//! One-shot seeding of the REAL daemon's environment-context store
//! (`~/.holoiroh/context/*.md` + `~/.holoiroh/context.db`) with the facts this feature was
//! built to fix. Idempotent: `EnvContextStore::remember` upserts by key, so re-running this
//! is safe and just refreshes `updated_at_ms`.
//!
//! Run: `cargo run --example env_context_seed`

use anyhow::{Context, Result};
use holoiroh_daemon::env_context::EnvContextStore;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let store = EnvContextStore::open().context("opening the real daemon's env_context store")?;

    let facts: &[(&str, &str)] = &[
        (
            "terminal-app-ghostty",
            "The user's terminal application is Ghostty, not Apple's Terminal.app or iTerm2. \
             When asked to open, use, or go to a terminal or an existing CLI session (e.g. \
             Claude Code), check for an already-running Ghostty window first (e.g. via \
             Mission Control / Cmd+Tab / the Dock) instead of opening a new terminal \
             application. Live-witnessed failure this fact fixes: asked to 'go to Claude Code' \
             to modify a project, the agent opened a brand new terminal instead of finding the \
             Claude Code session already running in an existing Ghostty window.",
        ),
        (
            "project-aro-holoiroh",
            "This computer-use daemon's own source code project is called Aro (internal \
             codename holoiroh, directory ~/Documents/agentOS/holoiroh); it is a git \
             repository the user edits directly via Claude Code, typically already running in \
             an existing Ghostty terminal window rather than needing a fresh one opened.",
        ),
    ];

    for (key, text) in facts {
        store.remember(key, text).await.with_context(|| format!("remembering {key}"))?;
        println!("remembered: {key}");
    }

    println!("seeded {} fact(s) into the real ~/.holoiroh/context store", facts.len());
    Ok(())
}
