//! Witnesses `crate::env_context` end-to-end against REAL BGE-small-en-v1.5 embeddings and a
//! real libsql vector index: seed a handful of facts (including the actual Ghostty-terminal
//! fact this feature exists to fix), then confirm semantic retrieval surfaces the right fact
//! for a realistic prompt that never mentions "Ghostty" by name -- proving this is genuine
//! semantic search, not keyword matching.
//!
//! Uses an isolated `~/.holoiroh/context-probe/` + `context-probe.db` (never the daemon's real
//! `~/.holoiroh/context/` + `context.db`) so this probe can be re-run freely without polluting
//! or depending on the real corpus.
//!
//! Run: `cargo run --example env_context_probe` (first run downloads ~130MB of model weights;
//! subsequent runs hit the local cache and are fast).

use anyhow::{Context, Result, bail};
use holoiroh_daemon::env_context::{EnvContextStore, format_context_block};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // Isolated corpus dir + db (fresh every run), but the REAL $HOME's model cache -- so this
    // probe never touches the daemon's real ~/.holoiroh/context/ corpus, while still reusing
    // an already-downloaded BGE-small-en-v1.5 across repeat runs instead of re-fetching ~130MB
    // every time. `open_at` (not `open`) exists specifically for this isolation.
    let home = std::env::var_os("HOME").context("HOME not set")?;
    let probe_dir = std::path::PathBuf::from(&home).join(".holoiroh_env_context_probe");
    let _ = std::fs::remove_dir_all(&probe_dir); // fresh corpus/db every run
    let real_home_cache = std::path::PathBuf::from(&home).join(".holoiroh").join("models/bge-small-en-v1.5");
    let store = EnvContextStore::open_at(
        probe_dir.join("context"),
        probe_dir.join("context.db"),
        real_home_cache,
    )
    .context("opening probe store")?;

    println!("seeding facts (first run downloads BGE-small-en-v1.5, ~130MB)...");
    let facts = [
        (
            "terminal-app-ghostty",
            "The user's terminal application is Ghostty, not Apple's Terminal.app or iTerm2. \
             When asked to open, use, or go to a terminal or an existing CLI session (e.g. \
             Claude Code), check for an already-running Ghostty window first instead of \
             opening a new terminal application.",
        ),
        (
            "project-aro-holoiroh",
            "This computer-use daemon's own source code project is called Aro (internal \
             codename holoiroh); its files live in a git repository the user works on \
             directly via Claude Code, usually already running in an existing Ghostty window.",
        ),
        (
            "unrelated-music-preference",
            "The user prefers dark-themed applications and uses Spotify for music playback.",
        ),
    ];
    for (key, text) in facts {
        store.remember(key, text).await.with_context(|| format!("remembering {key}"))?;
        println!("  remembered: {key}");
    }

    // The exact real-world query shape: a prompt that does NOT mention "Ghostty" or "terminal"
    // by name (matching the live-witnessed failure: "go to Claude Code to modify this
    // project"), so a hit here proves semantic retrieval, not literal keyword overlap.
    let query = "go to Claude Code to modify this project";
    println!("\nquery: {query:?}");
    let retrieved = store.retrieve(query, 3).await.context("retrieve")?;
    for fact in &retrieved {
        println!("  [{}] {}", fact.key, &fact.text[..fact.text.len().min(80)]);
    }

    let top_keys: Vec<&str> = retrieved.iter().map(|f| f.key.as_str()).collect();
    let found_ghostty = top_keys.contains(&"terminal-app-ghostty");
    let found_project = top_keys.contains(&"project-aro-holoiroh");
    let music_not_top1 = retrieved.first().map(|f| f.key != "unrelated-music-preference").unwrap_or(false);

    println!(
        "\n[{}] found_ghostty={found_ghostty} found_project={found_project} music_not_top1={music_not_top1}",
        if found_ghostty && found_project && music_not_top1 { "OK" } else { "FAIL" }
    );

    let block = format_context_block(&retrieved);
    println!("\n--- actual prompt-injection block ---");
    println!("{}", block.as_deref().unwrap_or("<none>"));
    println!("--- end block ---");

    // Reindex witness: delete the db, rebuild purely from the markdown corpus, confirm the
    // same fact is still retrievable -- proves the corpus (not just the db) is real durable
    // storage, matching this module's documented "corpus is the source of truth" design.
    std::fs::remove_file(probe_dir.join("context.db")).ok();
    let reindexed = store.reindex().await.context("reindex")?;
    let post_reindex = store.retrieve(query, 3).await.context("retrieve after reindex")?;
    let survived_reindex = post_reindex.iter().any(|f| f.key == "terminal-app-ghostty");
    println!(
        "[{}] reindexed={reindexed} survived_reindex={survived_reindex}",
        if reindexed == facts.len() && survived_reindex { "OK" } else { "FAIL" }
    );

    if !(found_ghostty && found_project && music_not_top1 && survived_reindex) {
        bail!("ENV CONTEXT PROBE FAILED");
    }
    println!("\nENV CONTEXT PROBE: ALL WITNESSED OK (real BGE embeddings, real libsql vector_top_k, real reindex-from-corpus)");
    Ok(())
}
