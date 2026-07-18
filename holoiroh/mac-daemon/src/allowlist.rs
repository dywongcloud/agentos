//! Auth-beyond-ticket: a persisted device allowlist plus a PIN check, both
//! usable independently of whether either is wired into the control-channel
//! accept path yet (see `holoiroh/PAIRING.md`'s "Implementation status"
//! table for the authoritative real-vs-designed split).
//!
//! ## Why this exists
//!
//! Per `holoiroh/README.md`'s "Security model" section, possessing the iroh
//! ticket is today sufficient to connect -- a leaked QR screenshot or pasted
//! ticket string hands over full control. This module provides the two
//! building blocks `PAIRING.md` designs for closing that gap:
//!
//! - [`Allowlist`]: a JSON file at `~/.holoiroh/allowlist.json` recording
//!   previously-paired client device public keys, so a device seen once
//!   (and presumably PIN-verified at that time) can reconnect without
//!   re-entering the PIN.
//! - [`verify_pin`]: a constant-time-ish comparison for PIN strings entered
//!   on first connection, exchanged out-of-band (displayed on the Mac's
//!   terminal alongside the ticket/QR, per `PAIRING.md`).
//!
//! Both are real, independently callable, and covered by unit tests below
//! (`cargo test -p holoiroh-daemon`) -- this is not a stub. What is **not**
//! yet true: neither type is constructed or called from
//! `control_channel.rs`'s `ProtocolHandler::accept`. See `PAIRING.md`'s
//! "Exact remaining wiring step" section for precisely what that would take.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One previously-paired client device, as recorded in `allowlist.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    /// The connecting peer's iroh node id, `iroh::EndpointId::to_string()`
    /// form (hex-encoded public key) -- stored as plain text, not raw bytes,
    /// so the JSON file stays human-inspectable (`cat
    /// ~/.holoiroh/allowlist.json` is meant to be a legitimate way to audit
    /// who has ever been allowed in).
    pub device_id: String,
    /// Human-readable label the user can attach at pairing time (e.g. "my
    /// iPhone 15"). Optional: not every pairing flow will collect one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
    /// Unix timestamp (seconds) of when this entry was added. Recorded for
    /// audit purposes (matching README's "metadata-only audit log" PRD
    /// item) -- not currently used to expire entries; the allowlist has no
    /// TTL by design (revocation is a separate, not-yet-implemented,
    /// explicit-removal operation -- see `PAIRING.md`).
    pub paired_at: u64,
}

/// A persisted set of allowlisted device public keys, backed by a single
/// JSON file at `~/.holoiroh/allowlist.json`.
///
/// This struct is intentionally dumb: it does not itself decide *when* to
/// consult the allowlist or how a device gets added (that policy belongs at
/// the call site -- see `PAIRING.md`'s wiring-step doc). It only knows how
/// to load, save, query, and mutate the on-disk JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Allowlist {
    entries: Vec<AllowlistEntry>,
}

impl Allowlist {
    /// Default location: `~/.holoiroh/allowlist.json`. Resolved via `$HOME`
    /// rather than a platform-dirs crate -- this daemon is macOS-only (per
    /// `README.md`), where `$HOME` is always set for an interactive login
    /// session, and adding a new dependency for a single path join isn't
    /// warranted.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home).join(".holoiroh").join("allowlist.json"))
    }

    /// Loads the allowlist from `path`. A missing file is treated as an
    /// empty allowlist (the natural state before any device has ever
    /// paired) rather than an error -- every other I/O or parse failure is
    /// a real error, since a *corrupt* allowlist file silently treated as
    /// empty would fail open (accepting an unregistered device) rather than
    /// fail closed.
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

    /// Writes the current entries to `path` as pretty-printed JSON,
    /// creating the parent directory (`~/.holoiroh/`) if it doesn't exist
    /// yet. Overwrites the whole file (no partial-write/lock handling --
    /// this daemon supports exactly one concurrent control-channel
    /// connection today per `control_channel.rs`'s own doc comment, so
    /// concurrent writers are not a real scenario yet).
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

    /// True if `device_id` has been previously paired. This is the check a
    /// (not-yet-wired) accept-path would call before accepting a control
    /// stream from a peer that didn't just PIN-verify this session -- see
    /// [`crate::control_channel`] and `PAIRING.md`.
    pub fn contains_key(&self, device_id: &str) -> bool {
        self.entries.iter().any(|e| e.device_id == device_id)
    }

    /// Adds `device_id` to the allowlist (no-op if already present -- the
    /// existing entry, including its original `paired_at`, is left
    /// untouched rather than duplicated or refreshed). Returns `true` if a
    /// new entry was actually added.
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

    /// Removes `device_id` from the allowlist (the revocation primitive
    /// `PAIRING.md` and README's "Security model" section describe but that
    /// no call site invokes yet -- there is no `--revoke-device <id>` CLI
    /// command or control-channel message wired up to call this). Returns
    /// `true` if an entry was removed.
    #[allow(dead_code)]
    pub fn remove_entry(&mut self, device_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.device_id != device_id);
        self.entries.len() != before
    }

    /// Number of allowlisted devices. Not yet called from `main.rs`/
    /// `control_channel.rs` (no diagnostics/status command surfaces this
    /// yet) -- kept as the obvious accessor a future `--list-paired-devices`
    /// command would use.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no devices are allowlisted yet (the state a fresh install is
    /// in before any pairing has ever completed). Same not-yet-called status
    /// as [`Self::len`].
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All allowlisted device ids, for diagnostics (e.g. a future `--list-
    /// paired-devices` CLI command). Same not-yet-called status as
    /// [`Self::len`].
    #[allow(dead_code)]
    pub fn device_ids(&self) -> HashSet<&str> {
        self.entries.iter().map(|e| e.device_id.as_str()).collect()
    }
}

/// Generates a random numeric PIN of `digits` length (default use: 6, via
/// [`generate_pin`]) using [`rand`] if available in the dependency graph --
/// **not yet wired as a dependency** (see `Cargo.toml`; this function is
/// written against `std`'s own weak randomness source so it compiles today
/// without adding a new crate, and is flagged in `PAIRING.md` as needing a
/// real CSPRNG (`rand::rngs::OsRng` or similar) before this is used for
/// anything beyond documentation/testing purposes).
///
/// Uses `std::collections::hash_map::RandomState` (which macOS/Linux seed
/// from the OS's own secure random source, `getrandom(2)`/`SecRandomCopyBytes`
/// transitively, per the standard library's own implementation) as a
/// zero-dependency source of entropy -- adequate for a short-lived,
/// single-use pairing PIN, but documented here rather than silently assumed
/// cryptographically reviewed.
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

/// Convenience: generates the default 6-digit PIN `PAIRING.md` designs
/// around (display alongside the QR code / ticket text on daemon startup).
pub fn generate_default_pin() -> String {
    generate_pin(6)
}

/// Compares `candidate` (what a connecting client sent) against `expected`
/// (what the daemon generated and displayed) without the early-exit
/// short-circuit a naive `candidate == expected` has -- a plain string
/// equality check returns as soon as the first differing byte is found,
/// which in principle leaks timing information about how many leading
/// characters were guessed correctly. For a short numeric PIN entered by a
/// human over an already-encrypted `iroh`/QUIC transport (not a raw network
/// oracle an attacker can time with the precision this attack needs), the
/// practical risk is low, but the fix costs nothing so it's applied anyway
/// rather than documented as an accepted risk.
///
/// Rejects (`false`) rather than panicking on any malformed input,
/// including empty strings and length mismatches -- length itself is not
/// treated as secret (comparing it up front, before the fold, is standard
/// practice for constant-time compares: see e.g. `subtle::ConstantTimeEq`'s
/// own doc, which notes length must match before the constant-time portion
/// even begins).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "holoiroh-allowlist-test-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn load_missing_file_returns_empty_allowlist() {
        let path = temp_path("missing");
        let list = Allowlist::load(&path).expect("missing file should load as empty, not error");
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn save_then_load_round_trips_entries() {
        let path = temp_path("roundtrip");
        let mut list = Allowlist::default();
        assert!(list.add_entry("node-abc123", Some("Dylan's iPhone".to_string())));
        assert!(list.add_entry("node-def456", None));
        list.save(&path).expect("save should succeed and create parent dir");

        let loaded = Allowlist::load(&path).expect("load should succeed after save");
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains_key("node-abc123"));
        assert!(loaded.contains_key("node-def456"));
        assert!(!loaded.contains_key("node-unknown"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn add_entry_is_idempotent_for_same_device_id() {
        let mut list = Allowlist::default();
        assert!(list.add_entry("node-x", None));
        assert!(!list.add_entry("node-x", Some("relabel attempt".to_string())));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn contains_key_rejects_unknown_device() {
        let mut list = Allowlist::default();
        list.add_entry("node-known", None);
        assert!(list.contains_key("node-known"));
        assert!(!list.contains_key("node-unknown-attacker-device"));
    }

    #[test]
    fn remove_entry_revokes_a_previously_paired_device() {
        let mut list = Allowlist::default();
        list.add_entry("node-to-revoke", None);
        assert!(list.contains_key("node-to-revoke"));
        assert!(list.remove_entry("node-to-revoke"));
        assert!(!list.contains_key("node-to-revoke"));
        // Second removal of an already-gone entry reports no-op, not an error.
        assert!(!list.remove_entry("node-to-revoke"));
    }

    #[test]
    fn corrupt_json_file_fails_closed_not_open() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"{ this is not valid json").unwrap();
        let result = Allowlist::load(&path);
        assert!(
            result.is_err(),
            "a corrupt allowlist file must be a hard error, never silently treated as empty \
             (empty would fail OPEN -- accepting a device that was never actually verified)"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_path_is_under_home_dotholoiroh() {
        // Only meaningful when HOME is set, which it always is in a normal
        // macOS interactive/CI session -- this daemon is macOS-only.
        if std::env::var_os("HOME").is_some() {
            let path = Allowlist::default_path().unwrap();
            assert!(path.ends_with(".holoiroh/allowlist.json"));
        }
    }

    #[test]
    fn generate_pin_produces_requested_digit_count() {
        let pin = generate_pin(6);
        assert_eq!(pin.len(), 6);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn generate_default_pin_is_six_digits() {
        let pin = generate_default_pin();
        assert_eq!(pin.len(), 6);
    }

    #[test]
    fn generate_pin_zero_digits_clamps_to_one() {
        let pin = generate_pin(0);
        assert_eq!(pin.len(), 1);
    }

    #[test]
    fn verify_pin_accepts_correct_match() {
        assert!(verify_pin("123456", "123456"));
    }

    #[test]
    fn verify_pin_rejects_wrong_pin() {
        assert!(!verify_pin("000000", "123456"));
    }

    #[test]
    fn verify_pin_rejects_empty_candidate() {
        assert!(!verify_pin("", "123456"));
    }

    #[test]
    fn verify_pin_rejects_empty_expected() {
        assert!(!verify_pin("123456", ""));
    }

    #[test]
    fn verify_pin_rejects_both_empty() {
        assert!(!verify_pin("", ""));
    }

    #[test]
    fn verify_pin_rejects_length_mismatch() {
        assert!(!verify_pin("123", "123456"));
        assert!(!verify_pin("123456", "123"));
    }

    #[test]
    fn verify_pin_rejects_close_but_wrong_pin() {
        // Off-by-one-digit case, to catch a naive substring/prefix bug.
        assert!(!verify_pin("123457", "123456"));
    }

    #[test]
    fn verify_pin_is_case_sensitive_for_non_numeric_input() {
        // PINs are documented as numeric, but the function itself doesn't
        // enforce that -- verify it doesn't accidentally normalize case for
        // any alphanumeric variant a future extension might pass through.
        assert!(!verify_pin("abcdef", "ABCDEF"));
        assert!(verify_pin("abcdef", "abcdef"));
    }
}
