//! Persisted client-device allowlist and first-connection PIN helpers.
//!
//! `control_channel.rs` enforces this allowlist on every authenticated iroh
//! connection. A correct pre-session PIN adds the transport's complete endpoint
//! ID; later connections must match that exact 64-character lowercase value.
//! `mac-daemon/examples/allowlist_probe.rs` and `auth_gate_probe.rs` are the
//! executable witnesses. Legacy short or malformed IDs are quarantined during
//! load before the active list is used.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
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

const DEVICE_ID_HEX_LEN: usize = 64;
const MIGRATION_BACKUP_SUFFIX: &str = ".invalid-device-ids-v1.json";

/// Returns true only for a complete lowercase iroh endpoint identifier.
pub fn is_valid_device_id(device_id: &str) -> bool {
    device_id.len() == DEVICE_ID_HEX_LEN
        && device_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// Returns the deterministic sibling file used to quarantine legacy invalid IDs.
pub fn migration_backup_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("allowlist.json");
    path.with_file_name(format!("{file_name}{MIGRATION_BACKUP_SUFFIX}"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating allowlist directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("allowlist.json");
    let temp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));

    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| {
                format!("creating temporary allowlist file {}", temp_path.display())
            })?;
        file.write_all(bytes)
            .with_context(|| format!("writing temporary allowlist file {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing temporary allowlist file {}", temp_path.display()))?;
        std::fs::rename(&temp_path, path).with_context(|| {
            format!(
                "atomically replacing allowlist file {} from {}",
                path.display(),
                temp_path.display()
            )
        })?;
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn write_allowlist(path: &Path, list: &Allowlist) -> Result<()> {
    let json = serde_json::to_vec_pretty(list).context("serializing allowlist")?;
    atomic_write(path, &json)
}

fn migrate_invalid_entries(path: &Path, list: Allowlist) -> Result<Allowlist> {
    let (valid, invalid): (Vec<_>, Vec<_>) = list
        .entries
        .into_iter()
        .partition(|entry| is_valid_device_id(&entry.device_id));
    if invalid.is_empty() {
        return Ok(Allowlist { entries: valid });
    }

    let backup_path = migration_backup_path(path);
    let mut quarantined = match std::fs::read(&backup_path) {
        Ok(bytes) => serde_json::from_slice::<Allowlist>(&bytes).with_context(|| {
            format!(
                "parsing existing allowlist migration backup at {}",
                backup_path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Allowlist::default(),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "reading existing allowlist migration backup at {}",
                    backup_path.display()
                )
            });
        }
    };
    for entry in invalid {
        if !quarantined.entries.contains(&entry) {
            quarantined.entries.push(entry);
        }
    }

    // The backup lands first. If replacing the active list then fails, the
    // original active file remains intact and every invalid entry is already
    // preserved for the next deterministic retry.
    write_allowlist(&backup_path, &quarantined)?;
    let active = Allowlist { entries: valid };
    write_allowlist(path, &active)?;
    Ok(active)
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
    /// allowlist. Every other I/O or parse failure fails closed.
    ///
    /// On the first load of a legacy file, entries whose device IDs are not
    /// complete 64-character lowercase hex endpoint IDs are atomically moved
    /// to [`migration_backup_path`]. The active file is atomically rewritten
    /// with every valid entry preserved. A later load is idempotent because no
    /// invalid entries remain active.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(bytes) => {
                let list: Allowlist = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing allowlist JSON at {}", path.display()))?;
                migrate_invalid_entries(path, list)
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

    /// Atomically writes the current entries to `path` as pretty JSON.
    /// Every active entry must contain a complete lowercase endpoint ID.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| !is_valid_device_id(&entry.device_id))
        {
            bail!(
                "refusing to save invalid device id to active allowlist: {}",
                entry.device_id
            );
        }
        write_allowlist(path, self)
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

    /// Returns true only for an exact, complete endpoint-ID match. Legacy
    /// short prefixes and malformed IDs never authenticate.
    pub fn contains_key(&self, device_id: &str) -> bool {
        is_valid_device_id(device_id)
            && self
                .entries
                .iter()
                .any(|entry| entry.device_id == device_id)
    }

    /// Adds a complete lowercase endpoint ID. Invalid and duplicate IDs are
    /// rejected without changing the list.
    pub fn add_entry(&mut self, device_id: impl Into<String>, label: Option<String>) -> bool {
        let device_id = device_id.into();
        if !is_valid_device_id(&device_id) || self.contains_key(&device_id) {
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
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
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
