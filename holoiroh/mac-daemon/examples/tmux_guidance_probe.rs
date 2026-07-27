use holoiroh_daemon::tmux::{guidance_for, SessionState, HOST_TERMINAL_APP, SESSION_NAME, WINDOW_TITLE};

fn main() {
    assert!(
        guidance_for(&SessionState::TmuxNotInstalled).is_none(),
        "REGRESSION: with tmux absent the daemon would still instruct the agent to use a session \
         that cannot exist, instead of falling back to plain-terminal guidance"
    );
    println!("tmux absent -> no tmux guidance (falls back to the plain-terminal rules)");

    let attached = guidance_for(&SessionState::RunningWithAttachedWindow { attached_clients: 1 })
        .expect("a running session must produce guidance");
    let detached = guidance_for(&SessionState::RunningWithNoWindowAttached)
        .expect("a session with no window still needs guidance");

    for (label, text) in [("attached", &attached), ("no-window", &detached)] {
        assert!(
            text.contains(SESSION_NAME),
            "{label} guidance never names the session the agent should use"
        );
        assert!(
            text.contains(HOST_TERMINAL_APP),
            "{label} guidance does not name the host app to bring forward -- a bare app activate \
             raises whichever window was last focused, which is how the user's Claude Code window \
             got surfaced instead of the session"
        );
        assert!(
            text.contains("front"),
            "{label} guidance does not tell the agent to bring the window forward -- tmux \
             reporting `attached` is NOT visibility when a fullscreen app owns the macOS Space"
        );
        assert!(
            text.contains("Claude Code") && text.contains("Ghostty"),
            "{label} guidance drops the rule that Ghostty hosts the user's Claude Code sessions \
             and must be left alone"
        );
        assert!(
            text.contains("kill-server"),
            "{label} guidance does not forbid killing the server that holds the user's view"
        );
        println!("{label} guidance carries session, host app, raise, Claude-Code and kill rules");
    }

    assert!(
        detached.contains("tmux attach"),
        "with no window attached the agent is not told how to bring the session back into view"
    );
    assert_ne!(
        attached, detached,
        "guidance must not claim a window is already showing the session when none is"
    );

    assert!(
        WINDOW_TITLE.contains(SESSION_NAME),
        "the window title does not identify which session it hosts"
    );

    let protected = holoiroh_daemon::process_awareness::format_guard_block(&[]);
    assert!(
        protected.contains("Ghostty") && protected.contains("Claude Code"),
        "the hard guard block lost its terminal/Claude-Code rules"
    );

    let facts = holoiroh_daemon::env_context::DEFAULT_ENV_FACTS;
    let fact = facts
        .iter()
        .find(|(k, _)| *k == "terminal-work-in-tmux-session-aro")
        .expect("the semantic fact layer has no tmux fact, so retrieval can surface only the older prefer-a-Ghostty-window fact");
    assert!(
        fact.1.contains("live screen share"),
        "the tmux fact does not explain WHY visibility matters"
    );
    assert!(
        fact.1.contains("Claude Code"),
        "the tmux fact does not preserve the never-disturb-Claude-Code constraint"
    );
    println!("env fact present and consistent: {}", fact.0);

    println!("VERDICT: OK -- guidance is correct in every session state, with no machine access");
}
