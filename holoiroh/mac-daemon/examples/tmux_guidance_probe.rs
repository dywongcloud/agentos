use holoiroh_daemon::tmux::{
    terminal_work_guidance_for, SessionState, HOST_TERMINAL_APP, SESSION_NAME, WINDOW_TITLE,
};

const STATES: &[(&str, SessionState)] = &[
    (
        "attached",
        SessionState::RunningWithAttachedWindow { attached_clients: 1 },
    ),
    ("no-window", SessionState::RunningWithNoWindowAttached),
    ("tmux-absent", SessionState::TmuxNotInstalled),
];

const ROUTES_AGENT_INTO_GHOSTTY: &[&str] = &[
    "prefer an already-open Ghostty",
    "already-open Ghostty window",
    "already-running Ghostty window first",
    "check for an already-running Ghostty",
];

fn main() {
    for (label, state) in STATES {
        let text = terminal_work_guidance_for(state);
        println!("\n--- {label} ---\n{text}");

        assert!(
            text.contains(HOST_TERMINAL_APP),
            "{label}: guidance never names {HOST_TERMINAL_APP}, so the agent has no destination \
             for its own terminal work"
        );
        assert!(
            text.contains("front"),
            "{label}: guidance does not tell the agent to raise the window -- tmux reporting \
             `attached` is NOT visibility when a fullscreen app owns the macOS Space"
        );
        for phrase in ROUTES_AGENT_INTO_GHOSTTY {
            assert!(
                !text.contains(phrase),
                "{label}: guidance contains {phrase:?}, which routes the agent's own terminal \
                 work into a Ghostty window -- the exact windows that host the user's live \
                 Claude Code sessions"
            );
        }
        if !matches!(state, SessionState::TmuxNotInstalled) {
            assert!(
                text.contains(SESSION_NAME) && text.contains("kill-server"),
                "{label}: guidance must name the session and forbid killing it"
            );
        }
    }

    let guard = holoiroh_daemon::process_awareness::format_guard_block(&[]);
    println!("\n--- guard block ---\n{guard}");

    for phrase in ROUTES_AGENT_INTO_GHOSTTY {
        assert!(
            !guard.contains(phrase),
            "the HARD guard block still contains {phrase:?}. It also carries the tmux rule, so \
             the agent would receive two contradicting non-negotiable instructions about where \
             its terminal work goes -- and the Ghostty branch walks into the user's Claude Code \
             windows"
        );
    }
    assert!(
        guard.contains("Ghostty"),
        "the guard block must still identify Ghostty so the agent recognises and protects it"
    );
    assert!(
        guard.contains("never type into one") || guard.contains("never reuse one"),
        "the guard block no longer tells the agent to leave Ghostty windows alone"
    );
    assert!(
        guard.contains("NEVER interrupt") && guard.contains("Claude Code"),
        "the never-interrupt-Claude-Code guarantee did not survive the terminal-rule rewrite"
    );
    assert_eq!(
        guard.matches("Do your OWN terminal/CLI work").count(),
        1,
        "the guard block must give exactly ONE instruction about where the agent's terminal work \
         goes; more than one is the contradiction returning"
    );

    let facts = holoiroh_daemon::env_context::DEFAULT_ENV_FACTS;
    let ghostty = facts
        .iter()
        .find(|(k, _)| *k == "terminal-app-ghostty")
        .expect("the Ghostty identification fact must keep its key -- renaming it orphans the old contradicting text in ~/.holoiroh/context/ forever");
    let tmux_fact = facts
        .iter()
        .find(|(k, _)| *k == "terminal-work-in-tmux-session-aro")
        .expect("the tmux destination fact is missing");

    for phrase in ROUTES_AGENT_INTO_GHOSTTY {
        assert!(
            !ghostty.1.contains(phrase),
            "the seeded terminal-app-ghostty fact still contains {phrase:?}; semantic retrieval \
             can surface it ALONE, steering the agent into Ghostty with the tmux fact never shown"
        );
    }
    assert!(
        ghostty.1.contains("Claude Code") && ghostty.1.contains("Never type into"),
        "the Ghostty fact must still protect the user's windows"
    );
    assert!(
        ghostty.1.contains(SESSION_NAME),
        "the Ghostty fact should point at the session that IS the agent's destination, so a \
         retrieval that surfaces only this fact still routes the agent correctly"
    );
    assert!(
        tmux_fact.1.contains("live screen share"),
        "the tmux fact does not explain WHY visibility matters"
    );
    println!("\nenv facts consistent: {} + {}", ghostty.0, tmux_fact.0);

    assert!(
        WINDOW_TITLE.contains(SESSION_NAME),
        "the window title does not identify which session it hosts"
    );

    println!("\nVERDICT: OK -- one destination for the agent's terminal work, in every state, and Ghostty is protected rather than nominated");
}
