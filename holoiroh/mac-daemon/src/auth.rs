//! Startup check for an existing Holo auth token.
//!
//! `holo-desktop-cli` (the `holo` CLI) stores its auth token at
//! `~/.holo/.env` as a `HAI_API_KEY=...` line, written by `holo login`
//! (browser sign-in flow against portal.hcompany.ai). This daemon shells
//! out to `holo serve` (see `crate::holo_serve`) and therefore depends on
//! that login having already happened -- if it hasn't, `holo serve` would
//! itself fail confusingly (or worse, partially start) rather than giving
//! the user a clear "you forgot a step" message. We check for the token
//! file ourselves, up front, so the daemon never proceeds into that
//! broken state.

use std::fmt;
use std::path::PathBuf;

/// Why the Holo auth token check failed.
#[derive(Debug)]
pub enum AuthCheckError {
    /// `$HOME` is unset/empty in this process's environment -- can't even
    /// compute where `~/.holo/.env` would live.
    NoHomeDir,
    /// `~/.holo/.env` does not exist at all -- the user has never run
    /// `holo login`.
    MissingTokenFile { path: PathBuf },
    /// `~/.holo/.env` exists but could not be read (permissions, I/O
    /// error, etc.) -- distinct from "missing" so the instruction can be
    /// accurate.
    UnreadableTokenFile { path: PathBuf, source: std::io::Error },
    /// `~/.holo/.env` exists and was readable, but is empty or does not
    /// contain a `HAI_API_KEY=` line -- login was started but never
    /// completed, or the file was truncated/corrupted.
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
    /// User-facing remediation text. Always points at the exact command
    /// to run, never a generic "check your setup" message.
    pub fn remediation(&self) -> &'static str {
        "Run 'holo login' first, then try again."
    }
}

/// A successfully-located, non-empty Holo API key. The value itself is
/// intentionally not exposed via `Debug`/`Display` so it doesn't end up in
/// logs by accident.
pub struct HoloToken {
    api_key: String,
    path: PathBuf,
}

impl HoloToken {
    /// The resolved key value. Not yet called from `main.rs` (`holo serve` inherits
    /// `HAI_API_KEY` from the parent process's environment directly -- see
    /// `holo_bridge::process` and this crate's `Cargo.toml` comment on `dotenvy` -- so nothing
    /// today needs the parsed value threaded through explicitly). Kept as the natural accessor
    /// a future caller that must pass the key explicitly (rather than relying on inherited env)
    /// would need, same status as the `#[allow(dead_code)]` convenience methods in
    /// `allowlist.rs`.
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
/// Deliberately minimal: this is a single-purpose reader for the one key
/// this daemon needs, not a general `.env` parser. Handles the common
/// `.env` conventions this file is documented to use: `KEY=value` lines,
/// optional surrounding whitespace, `#`-prefixed comment lines, and
/// optional matching single/double quotes around the value.
///
/// `pub` (rather than private) so `examples/auth_probe.rs` -- a real,
/// run-by-hand live witness for this parsing logic (see this repo's
/// no-unit-tests rule) -- can call the actual function instead of a
/// reimplemented copy of it.
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

/// Check for an existing Holo auth token, in the same precedence order
/// `holo-desktop-cli` itself documents: a local `.env` (already loaded into
/// process env by `main.rs`'s `dotenvy::dotenv()` call before this runs),
/// then `~/.holo/.env` (written by `holo login`'s browser-OAuth flow), then
/// a bare already-exported process env var (covered by the same process-env
/// check as the first case, since `dotenvy` only ever *adds* to it).
///
/// This is the startup gate: call it before doing anything else (spawning
/// `holo serve`, touching the network, checking permissions). On success,
/// returns the parsed token. On failure, the caller should print
/// `error` + `error.remediation()` to stderr and exit non-zero -- never
/// proceed into a broken state where `holo serve` is spawned without a
/// valid token behind it.
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

/// Same as [`check_holo_token`] but with an explicit home directory --
/// the seam that makes this testable without mutating the real `$HOME`.
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

/// Resolve the current user's home directory the same way
/// `holo-desktop-cli`'s own `~/.holo/.env` convention implies: `$HOME` on
/// Unix. This daemon is macOS-only, so `$HOME` is authoritative here (no
/// extra crate dependency needed for a single env var read).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
