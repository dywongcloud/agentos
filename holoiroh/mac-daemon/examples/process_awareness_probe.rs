//! Manual, run-by-hand probe: witnesses `process_awareness::enumerate` against this Mac's REAL
//! running processes and prints the exact hard guard block that gets prepended to every turn --
//! the issue-2 "never interrupt Claude Code / default terminal is Ghostty / know what's running"
//! wiring, executed for real.
//!
//! Run with `cargo run --example process_awareness_probe`.

use holoiroh_daemon::process_awareness;

fn main() {
    let procs = process_awareness::enumerate();
    println!("enumerated {} running processes", procs.len());
    assert!(!procs.is_empty(), "ps returned no processes -- enumeration is broken");

    let protected: Vec<_> = procs.iter().filter(|p| p.protected).collect();
    println!("protected processes ({}):", protected.len());
    for p in &protected {
        println!("  pid={} comm={} -- {}", p.pid, p.comm, p.protected_reason);
    }

    println!();
    println!("=== the guard block injected into EVERY prompt ===");
    let block = process_awareness::format_guard_block(&procs);
    println!("{block}");

    // The hard rules are unconditional -- always present regardless of what's running.
    assert!(block.contains("Ghostty"), "guard must state the default terminal is Ghostty");
    assert!(
        block.to_lowercase().contains("never interrupt")
            || block.contains("NEVER interrupt"),
        "guard must forbid interrupting Claude Code"
    );
    assert!(block.contains("Claude Code"), "guard must name Claude Code explicitly");

    println!(
        "process_awareness_probe: OK -- real process enumeration + unconditional guard block \
         witnessed via real execution."
    );
}
