//! This is the startup check for an existing Holo auth token.
//!
//! `holo-desktop-cli` (the `holo` CLI) stores its auth token at
//! `~/.holo/.env`, as a `HAI_API_KEY=...` line. `holo login` writes this
//! line, through a browser sign-in flow against portal.hcompany.ai. This
//! daemon shells out to `holo serve` (see `crate::holo_serve`). This daemon
//! therefore depends on that login happening first. If the user has not
//! logged in, `holo serve` fails confusingly. In some cases, `holo serve`
//! partially starts instead. Either way, the user does not see a clear "you
//! forgot a step" message. This module checks for the token file first, so
//! the daemon never proceeds into that broken state.

use std::fmt;
use std::path::PathBuf;

/// This enum lists why the Holo auth token check failed.
#[derive(Debug)]
pub enum AuthCheckError {
    /// `$HOME` is unset or empty in this process's environment. The daemon
    /// cannot compute where `~/.holo/.env` lives.
    NoHomeDir,
    /// `~/.holo/.env` does not exist. The user never ran `holo login`.
    MissingTokenFile { path: PathBuf },
    /// `~/.holo/.env` exists, but the daemon could not read it (a
    /// permissions error or I/O error, for example). This case is distinct
    /// from "missing", so the remediation message stays accurate.
    UnreadableTokenFile { path: PathBuf, source: std::io::Error },
    /// `~/.holo/.env` exists and the daemon can read it. The file is empty,
    /// or the file does not contain a `HAI_API_KEY=` line. Either the login
    /// started but never finished, or the file became truncated or
    /// corrupted.
    MissingApiKey { path: PathBuf },
    /// `~/.holo/.env` has a `HAI_API_KEY=` line but the value after `=` is
    /// empty.
    EmptyApiKey { path: PathBuf },
}

impl fmt::Display for AuthCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthCheckError::NoHomeDir => write!(
                f,
                "could not determine the home directory (HOME is unset) -- \
                 cannot locate ~/.holo/.env"
            ),
            AuthCheckError::MissingTokenFile { path } => write!(
                f,
                "no Holo auth token found at {} -- you are not logged in",
                path.display()
            ),
            AuthCheckError::UnreadableTokenFile { path, source } => write!(
                f,
                "found {} but could not read it ({source}) -- check file \
                 permissions",
                path.display()
            ),
            AuthCheckError::MissingApiKey { path } => write!(
                f,
                "{} exists but has no HAI_API_KEY entry -- login did not \
                 complete successfully",
                path.display()
            ),
            AuthCheckError::EmptyApiKey { path } => write!(
                f,
                "{} has a HAI_API_KEY entry but the value is empty -- login \
                 did not complete successfully",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AuthCheckError {}

impl AuthCheckError {
    /// This is the user-facing remediation text. This function always
    /// points at the exact command to run. This function never returns a
    /// generic "check your setup" message.
    pub fn remediation(&self) -> &'static str {
        "Run 'holo login' first, then try again."
    }
}

/// This struct holds a successfully-located, non-empty Holo API key. This
/// struct deliberately does not expose the value via `Debug` or `Display`.
/// This prevents the value from ending up in logs by accident.
pub struct HoloToken {
    api_key: String,
    path: PathBuf,
}

impl HoloToken {
    /// This function returns the resolved key value. No code calls this
    /// function yet from `main.rs`. `holo serve` inherits `HAI_API_KEY`
    /// directly from the parent process's environment (see
    /// `holo_bridge::process` and this crate's `Cargo.toml` comment on
    /// `dotenvy`). So nothing today needs the parsed value threaded through
    /// explicitly. This function stays as the natural accessor for a future
    /// caller that must pass the key explicitly, instead of relying on
    /// inherited env. This function has the same status as the
    /// `#[allow(dead_code)]` convenience methods in `allowlist.rs`.
    #[allow(dead_code)]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl fmt::Debug for HoloToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HoloToken")
            .field("path", &self.path)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Resolve `~/.holo/.env` from the given home directory.
fn token_file_path(home: &std::path::Path) -> PathBuf {
    home.join(".holo").join(".env")
}

/// Parse a `HAI_API_KEY=...` line out of `.env`-style file contents.
///
/// This function is deliberately minimal. This function is a single-purpose
/// reader for the one key this daemon needs. This function is not a general
/// `.env` parser. This function handles the common `.env` conventions this
/// file is documented to use:
/// - `KEY=value` lines
/// - optional surrounding whitespace
/// - `#`-prefixed comment lines
/// - optional matching single or double quotes around the value
///
/// This function is `pub`, rather than private. This visibility lets
/// `examples/auth_probe.rs` call the actual function directly.
/// `examples/auth_probe.rs` is a real, run-by-hand live witness for this
/// parsing logic (see this repo's no-unit-tests rule). This visibility
/// avoids a reimplemented copy of the function inside
/// `examples/auth_probe.rs`.
pub fn extract_api_key(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "HAI_API_KEY" {
            continue;
        }
        let mut value = value.trim();
        if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
            || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
        {
            value = &value[1..value.len() - 1];
        }
        return Some(value.to_string());
    }
    None
}

/// Check for an existing Holo auth token. This function checks in the same
/// precedence order that `holo-desktop-cli` itself documents:
/// 1. a local `.env` (already loaded into process env by `main.rs`'s
///    `dotenvy::dotenv()` call, before this function runs)
/// 2. `~/.holo/.env` (written by `holo login`'s browser-OAuth flow)
/// 3. a bare, already-exported process env var (covered by the same
///    process-env check as the first case, since `dotenvy` only ever adds
///    to the environment)
///
/// This function is the startup gate. Call this function before doing
/// anything else. Call it before spawning `holo serve`, before touching the
/// network, and before checking permissions. Never let `holo serve` run
/// without a valid token behind it. On success, this function returns the
/// parsed token. On failure, the caller must print `error` and
/// `error.remediation()` to stderr, then exit non-zero.
pub fn check_holo_token() -> Result<HoloToken, AuthCheckError> {
    if let Ok(api_key) = std::env::var("HAI_API_KEY") {
        if !api_key.is_empty() {
            return Ok(HoloToken {
                api_key,
                path: PathBuf::from("$HAI_API_KEY (process env / local .env)"),
            });
        }
    }
    check_holo_token_in(&home_dir().ok_or(AuthCheckError::NoHomeDir)?)
}

/// This function behaves the same as [`check_holo_token`], but takes an
/// explicit home directory. This parameter is the seam that makes this
/// function testable without mutating the real `$HOME`.
pub fn check_holo_token_in(home: &std::path::Path) -> Result<HoloToken, AuthCheckError> {
    let path = token_file_path(home);

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuthCheckError::MissingTokenFile { path });
        }
        Err(source) => {
            return Err(AuthCheckError::UnreadableTokenFile { path, source });
        }
    };

    match extract_api_key(&contents) {
        None => Err(AuthCheckError::MissingApiKey { path }),
        Some(key) if key.is_empty() => Err(AuthCheckError::EmptyApiKey { path }),
        Some(api_key) => Ok(HoloToken { api_key, path }),
    }
}

/// Resolve the current user's home directory. This function uses the same
/// convention that `holo-desktop-cli`'s own `~/.holo/.env` path implies:
/// `$HOME` on Unix. This daemon is macOS-only, so `$HOME` is authoritative
/// here. This function needs no extra crate dependency for a single env var
/// read.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
