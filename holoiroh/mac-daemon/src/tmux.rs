use std::path::{Path, PathBuf};
use std::process::Command;

pub const SESSION_NAME: &str = "aro";

pub const HOST_TERMINAL_APP: &str = "Terminal";

const TMUX_SEARCH_PATHS: &[&str] = &[
    "/opt/homebrew/bin/tmux",
    "/usr/local/bin/tmux",
    "/usr/bin/tmux",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    TmuxNotInstalled,
    RunningWithAttachedWindow { attached_clients: usize },
    RunningWithNoWindowAttached,
}

impl SessionState {
    pub fn agent_can_use_it(&self) -> bool {
        matches!(self, SessionState::RunningWithAttachedWindow { .. })
    }
}

pub fn tmux_binary() -> Option<PathBuf> {
    TMUX_SEARCH_PATHS
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

fn tmux_reports_session_exists(tmux: &Path) -> bool {
    Command::new(tmux)
        .args(["has-session", "-t", SESSION_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn attached_client_count(tmux: &Path) -> usize {
    Command::new(tmux)
        .args(["list-clients", "-t", SESSION_NAME])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

pub const WINDOW_TITLE: &str = "Aro Shared Terminal (tmux:aro)";

fn create_detached_session(tmux: &Path) -> bool {
    let created = Command::new(tmux)
        .args(["new-session", "-d", "-s", SESSION_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if created {
        label_session_window(tmux);
    }
    created
}

fn label_session_window(tmux: &Path) {
    let _ = Command::new(tmux)
        .args(["set-option", "-t", SESSION_NAME, "set-titles", "on"])
        .status();
    let _ = Command::new(tmux)
        .args([
            "set-option",
            "-t",
            SESSION_NAME,
            "set-titles-string",
            WINDOW_TITLE,
        ])
        .status();
}

fn open_host_terminal_attached_to_session(tmux: &Path) -> bool {
    let attach = format!("{} attach -t {SESSION_NAME}", tmux.display());
    Command::new("osascript")
        .args([
            "-e",
            &format!("tell application \"{HOST_TERMINAL_APP}\" to do script \"{attach}\""),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn session_state() -> SessionState {
    let Some(tmux) = tmux_binary() else {
        return SessionState::TmuxNotInstalled;
    };
    if !tmux_reports_session_exists(&tmux) {
        return SessionState::RunningWithNoWindowAttached;
    }
    match attached_client_count(&tmux) {
        0 => SessionState::RunningWithNoWindowAttached,
        attached_clients => SessionState::RunningWithAttachedWindow { attached_clients },
    }
}

pub fn ensure_session() -> SessionState {
    let Some(tmux) = tmux_binary() else {
        return SessionState::TmuxNotInstalled;
    };

    if !tmux_reports_session_exists(&tmux) && !create_detached_session(&tmux) {
        return SessionState::RunningWithNoWindowAttached;
    }

    if attached_client_count(&tmux) == 0 {
        open_host_terminal_attached_to_session(&tmux);
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let attached_clients = attached_client_count(&tmux);
            if attached_clients > 0 {
                return SessionState::RunningWithAttachedWindow { attached_clients };
            }
        }
        return SessionState::RunningWithNoWindowAttached;
    }

    session_state()
}

pub fn guidance_line() -> Option<String> {
    guidance_for(&session_state())
}

pub fn guidance_for(state: &SessionState) -> Option<String> {
    let shared_rules = format!(
        "Bring the {HOST_TERMINAL_APP} window to the front before you work in it (click \
         {HOST_TERMINAL_APP} in the Dock, or Cmd+Tab to it) so the user watching the live screen \
         share can see the work happen -- a fullscreen app on another macOS Space hides it \
         completely, and work nobody can see defeats the point of sharing the screen. Do this \
         terminal work in {HOST_TERMINAL_APP}, never in Ghostty: Ghostty is the user's own \
         terminal and its windows host live Claude Code sessions you must never disturb. Never \
         `tmux kill-server`, `tmux kill-session`, or detach the session.\n"
    );
    match state {
        SessionState::TmuxNotInstalled => None,
        SessionState::RunningWithAttachedWindow { .. } => Some(format!(
            "- For terminal/CLI work, use the tmux session named `{SESSION_NAME}`, already \
             running in a {HOST_TERMINAL_APP} window. {shared_rules}"
        )),
        SessionState::RunningWithNoWindowAttached => Some(format!(
            "- For terminal/CLI work, use the tmux session named `{SESSION_NAME}`. No window is \
             showing it right now, so open {HOST_TERMINAL_APP} and run \
             `tmux attach -t {SESSION_NAME}` (create it with `tmux new-session -s {SESSION_NAME}` \
             if it is gone). {shared_rules}"
        )),
    }
}
