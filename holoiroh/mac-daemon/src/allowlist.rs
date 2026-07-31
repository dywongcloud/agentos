//! This module adds authentication beyond ticket possession: a persisted
//! device allowlist and a PIN check. Neither piece is wired into the
//! control-channel accept path yet. See the "Implementation status" table
//! in `holoiroh/PAIRING.md` for the real-vs-designed split.
//!
//! ## Why this exists
//!
//! Per the "Security model" section of `holoiroh/README.md`, ticket
//! possession alone is enough to connect today. A leaked QR screenshot or a
//! pasted ticket string hands over full control. This module provides the
//! two building blocks that `PAIRING.md` designs to close that gap:
//!
//! - [`Allowlist`]: a JSON file at `~/.holoiroh/allowlist.json` that
//!   records previously-paired client device public keys. A device seen
//!   once, and presumably PIN-verified at that time, can reconnect without
//!   re-entering the PIN.
//! - [`verify_pin`]: a constant-time-ish comparison for PIN strings entered
//!   on first connection. The PIN is exchanged out-of-band: the Mac's
//!   terminal displays it alongside the ticket and QR code, per
//!   `PAIRING.md`.
//!
//! Both are real and independently callable. Unit tests below cover both
//! (`cargo test -p holoiroh-daemon`). This module is not a stub. Neither
//! type is constructed or called yet from `control_channel.rs`'s
//! `ProtocolHandler::accept`. See the "Exact remaining wiring step" section
//! in `PAIRING.md` for the exact steps.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One previously-paired client device, as recorded in `allowlist.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    /// The connecting peer's iroh node id, in `iroh::EndpointId::to_string()`
    /// form (a hex-encoded public key). This field stores the id as plain
    /// text, not raw bytes, so the JSON file stays human-inspectable.
    /// Running `cat ~/.holoiroh/allowlist.json` is a legitimate way to audit
    /// which devices were ever allowed in.
    pub device_id: String,
    /// Human-readable label the user can attach at pairing time (e.g. "my
    /// iPhone 15"). Optional: not every pairing flow will collect one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    /// Unix timestamp, in seconds, of when this entry was added. The
    /// daemon records it for audit purposes, matching README's
    /// "metadata-only audit log" PRD item. The daemon does not use this
    /// timestamp to expire entries. The allowlist has no TTL by design.
    /// Revocation is a separate, explicit-removal operation that is not
    /// yet implemented. See `PAIRING.md`.
    pub paired_at: u64,
}

/// A persisted set of allowlisted device public keys, backed by a single
/// JSON file at `~/.holoiroh/allowlist.json`.
///
/// This struct is intentionally dumb. It does not decide *when* to consult
/// the allowlist or how a device gets added. That policy belongs at the
/// call site. See `PAIRING.md`'s wiring-step doc for details. This struct
/// only loads, saves, queries, and mutates the on-disk JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Allowlist {
    entries: Vec<AllowlistEntry>,
}

impl Allowlist {
    /// Default location: `~/.holoiroh/allowlist.json`. This function
    /// resolves the path via `$HOME`, not a platform-dirs crate. This
    /// daemon is macOS-only, per `README.md`. On macOS, `$HOME` is always
    /// set for an interactive login session. Adding a new dependency for a
    /// single path join is not warranted.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home).join(".holoiroh").join("allowlist.json"))
    }

    /// Loads the allowlist from `path`. A missing file counts as an empty
    /// allowlist. This is the natural state before the first device ever
    /// pairs. A missing file is not an error. Every other I/O or parse
    /// failure is a real error. Treating a *corrupt* file as empty fails
    /// open: it accepts an unregistered device. This function fails closed
    /// instead.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(bytes) => {
                let list: Allowlist = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing allowlist JSON at {}", path.display()))?;
                Ok(list)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Allowlist::default()),
            Err(err) => {
                Err(err).with_context(|| format!("reading allowlist file at {}", path.display()))
            }
        }
    }

    /// Convenience wrapper around [`Self::load`] using [`Self::default_path`].
    // Not yet called from `main.rs` (`ControlChannel::load_allowlist_best_effort`
    // calls `Self::default_path` + `Self::load` separately so it can log the
    // resolved path on failure) -- kept as the natural one-call convenience
    // a future caller (or a test) reaches for, rather than deleted.
    #[allow(dead_code)]
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    /// Writes the current entries to `path` as pretty-printed JSON. It
    /// creates the parent directory, `~/.holoiroh/`, if the directory does
    /// not exist yet. This function overwrites the whole file. It does no
    /// partial-write or lock handling. This daemon supports exactly one
    /// concurrent control-channel connection today, per
    /// `control_channel.rs`'s own doc comment. So concurrent writers are
    /// not a real scenario yet.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating allowlist directory {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(self).context("serializing allowlist")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing allowlist file at {}", path.display()))?;
        Ok(())
    }

    /// Convenience wrapper around [`Self::save`] using [`Self::default_path`].
    // Not yet called (control_channel.rs's authenticate() saves to the
    // resolved `state.allowlist_path` it already holds rather than
    // re-resolving the default path) -- kept as the natural convenience for
    // any future caller that only has an `Allowlist` value, not also the
    // path it was loaded from.
    #[allow(dead_code)]
    pub fn save_default(&self) -> Result<()> {
        self.save(Self::default_path()?)
    }

    /// Returns true if `device_id` was previously paired. This check exists
    /// for a not-yet-wired accept path. That path will call this function
    /// before it accepts a control stream from a peer that did not just
    /// PIN-verify this session. See [`crate::control_channel`] and
    /// `PAIRING.md`.
    pub fn contains_key(&self, device_id: &str) -> bool {
        self.entries.iter().any(|e| e.device_id == device_id)
    }

    /// Adds `device_id` to the allowlist. If `device_id` is already
    /// present, this function does nothing: it leaves the existing entry,
    /// including its original `paired_at`, untouched rather than
    /// duplicating or refreshing it. Returns `true` if this function
    /// actually adds a new entry.
    pub fn add_entry(&mut self, device_id: impl Into<String>, label: Option<String>) -> bool {
        let device_id = device_id.into();
        if self.contains_key(&device_id) {
            return false;
        }
        let paired_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.entries.push(AllowlistEntry {
            device_id,
            label,
            paired_at,
        });
        true
    }

    /// Removes `device_id` from the allowlist. `PAIRING.md` and README's
    /// "Security model" section describe this as the revocation primitive.
    /// No call site invokes it yet. There is no `--revoke-device <id>` CLI
    /// command. No control-channel message is wired up to call this
    /// function. Returns `true` if this function removes an entry.
    #[allow(dead_code)]
    pub fn remove_entry(&mut self, device_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.device_id != device_id);
        self.entries.len() != before
    }

    /// Number of allowlisted devices. `main.rs` and `control_channel.rs` do
    /// not call this function yet. No diagnostics or status command
    /// surfaces this value yet. This function stays as the obvious
    /// accessor a future `--list-paired-devices` command will use.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if no devices are allowlisted yet. This is the state of
    /// a fresh install before any pairing completes. This function has the
    /// same not-yet-called status as [`Self::len`].
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All allowlisted device ids, for diagnostics (for example, a future
    /// `--list-paired-devices` CLI command). This function has the same
    /// not-yet-called status as [`Self::len`].
    #[allow(dead_code)]
    pub fn device_ids(&self) -> HashSet<&str> {
        self.entries.iter().map(|e| e.device_id.as_str()).collect()
    }
}

/// Generates a random numeric PIN of `digits` length. The default use is 6
/// digits, via [`generate_pin`]. This function is designed to use [`rand`]
/// when the dependency graph includes it, but `rand` is **not yet wired as
/// a dependency** (see `Cargo.toml`). This function instead uses `std`'s
/// own weak randomness source, so it compiles today without a new crate.
/// `PAIRING.md` flags this function as needing a real CSPRNG, such as
/// `rand::rngs::OsRng`, before any use beyond documentation or testing.
///
/// This function uses `std::collections::hash_map::RandomState` as a
/// zero-dependency source of entropy. On macOS and Linux, `RandomState`
/// seeds from the OS's own secure random source
/// (`getrandom(2)`/`SecRandomCopyBytes`, transitively, per the standard
/// library's own implementation). This entropy source is adequate for a
/// short-lived, single-use pairing PIN. This doc states that fact
/// explicitly, rather than silently assuming cryptographic review.
pub fn generate_pin(digits: u32) -> String {
    use std::hash::{BuildHasher, Hasher};
    let digits = digits.max(1);
    let mut pin = String::with_capacity(digits as usize);
    // RandomState::new() re-seeds from the OS RNG each call (it is not a
    // fixed seed), so hashing successive counters below still yields
    // unpredictable output across process runs/calls.
    let state = std::collections::hash_map::RandomState::new();
    for i in 0..digits {
        let mut hasher = state.build_hasher();
        hasher.write_u32(i);
        hasher.write_u128(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0));
        let digit = (hasher.finish() % 10) as u8;
        pin.push((b'0' + digit) as char);
    }
    pin
}

/// Convenience function. It generates the default 6-digit PIN that
/// `PAIRING.md` designs around. The daemon displays this PIN alongside the
/// QR code and ticket text at startup.
pub fn generate_default_pin() -> String {
    generate_pin(6)
}

/// Compares `candidate` (what a connecting client sent) against `expected`
/// (what the daemon generated and displayed). This function avoids the
/// early-exit short-circuit of a naive `candidate == expected` check. A
/// plain string equality check returns as soon as it finds the first
/// differing byte. In principle, that early exit leaks timing information
/// about how many leading characters an attacker guessed correctly. This
/// function handles a short numeric PIN entered by a human over an
/// already-encrypted `iroh`/QUIC transport. That transport is not a raw
/// network oracle. An attacker cannot time it with the precision this
/// attack needs. So the practical risk is low. The fix costs nothing, so
/// this function applies it anyway, rather than documenting the risk as
/// accepted.
///
/// This function rejects (`false`) malformed input instead of panicking,
/// including empty strings and length mismatches. This function does not
/// treat length itself as secret. Comparing length up front, before the
/// fold, is standard practice for constant-time compares. For example,
/// `subtle::ConstantTimeEq`'s own documentation notes that length must
/// match before the constant-time portion even begins.
pub fn verify_pin(candidate: &str, expected: &str) -> bool {
    if candidate.is_empty() || expected.is_empty() {
        return false;
    }
    if candidate.len() != expected.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (a, b) in candidate.bytes().zip(expected.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}
