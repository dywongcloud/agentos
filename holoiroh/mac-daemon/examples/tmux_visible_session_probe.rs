use holoiroh_daemon::tmux::{
    SESSION_NAME, SessionState, ensure_session, session_state, terminal_work_guidance,
};

fn main() {
    println!("tmux binary: {:?}", holoiroh_daemon::tmux::tmux_binary());

    let first = ensure_session();
    println!("ensure #1 -> {first:?}");
    let second = ensure_session();
    println!("ensure #2 -> {second:?}");
    let third = session_state();
    println!("session_state after two ensures -> {third:?}");

    assert!(
        first.agent_can_use_it(),
        "ensure_session did not leave a session with an attached terminal window: {first:?}"
    );
    assert_eq!(
        first, second,
        "ensure_session is not idempotent: a second call changed state from {first:?} to {second:?}"
    );
    if let (
        SessionState::RunningWithAttachedWindow {
            attached_clients: a,
        },
        SessionState::RunningWithAttachedWindow {
            attached_clients: b,
        },
    ) = (&first, &second)
    {
        assert_eq!(
            a, b,
            "ensure_session opened an EXTRA terminal window on the second call ({a} -> {b}); \
             repeated ensures would bury the window the user is meant to watch"
        );
    }

    let live = terminal_work_guidance();
    println!("\n--- guidance the agent actually receives ---\n{live}");
    assert!(
        live.contains(SESSION_NAME),
        "guidance never names the session the agent is supposed to use"
    );

    let tmux = holoiroh_daemon::tmux::tmux_binary().expect("tmux is installed here");
    let title = std::process::Command::new(&tmux)
        .args([
            "show-options",
            "-t",
            SESSION_NAME,
            "-v",
            "set-titles-string",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("session window title: {title:?}");
    assert_eq!(
        title,
        holoiroh_daemon::tmux::WINDOW_TITLE,
        "the session does not label its own window, so the user cannot tell which terminal window \
         is Aro's -- and a title set by hand in one session is not a shipped behaviour"
    );

    let guard = holoiroh_daemon::process_awareness::guard_block_now();
    assert!(
        guard.contains(SESSION_NAME),
        "the per-turn hard guard block does not carry the tmux rule, so the agent never sees it"
    );
    println!("\nguard block carries the tmux rule: yes");

    let facts = holoiroh_daemon::env_context::DEFAULT_ENV_FACTS;
    let tmux_fact = facts
        .iter()
        .find(|(k, _)| *k == "terminal-work-in-tmux-session-aro")
        .expect("the semantic fact layer has no tmux fact, so retrieval can still surface only the older prefer-a-Ghostty-window fact");
    assert!(
        tmux_fact.1.contains("live screen share"),
        "the tmux fact does not explain WHY visibility matters"
    );
    println!("env fact present: {}", tmux_fact.0);

    println!(
        "\nVERDICT: OK -- one visible session, idempotent ensure, and both guidance layers agree"
    );
}
