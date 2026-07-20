//! Single-instance guard for the whole `holoiroh-daemon` process.
//!
//! Live-witnessed bug this exists to close: with no guard, two
//! `holoiroh-daemon` processes could run concurrently (e.g. an old
//! never-closed terminal window from a prior debugging session, plus a
//! freshly started one). Each publishes its own independent iroh broadcast
//! and prints its own valid-looking QR code/ticket/phrase -- but only ONE of
//! them can win `holo serve`'s own single-instance port bind (see
//! `holo_bridge::process`). The loser's `HoloBridge::start` fails, and
//! `main.rs` degrades that failure to a quiet `info!` log line with NO
//! control channel mounted on its router at all -- so a phone that happens
//! to scan the loser's QR code gets a real endpoint, a real ticket, a real
//! PIN prompt... and then ALPN negotiation error 120 ("peer doesn't support
//! any known protocol") on every control-channel dial, indistinguishable
//! from a crash to the end user. This guard prevents the ambiguous state
//! from ever existing: the second daemon refuses to start at all, with a
//! clear error naming the PID already holding the lock.
//!
//! Implementation: an `flock(2)` exclusive, non-blocking lock on a fixed
//! path under the OS temp dir. `flock` (unlike a plain PID-file existence
//! check) is automatically released by the kernel if the holding process
//! dies any way at all (crash, SIGKILL, power loss) -- no stale-lock cleanup
//! logic needed, which a hand-rolled "read PID, check if alive" scheme would
//! require and could itself race.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

fn lock_path() -> PathBuf {
    std::env::temp_dir().join("holoiroh-daemon.lock")
}

/// Holds the lock for the process lifetime; dropping releases it (also
/// released automatically by the kernel on process exit for any reason).
pub struct InstanceGuard {
    _file: File,
}

impl InstanceGuard {
    /// Acquires the single-instance lock or returns an error naming the PID
    /// already holding it (best-effort -- the PID recorded in the file is
    /// informational only, never used for liveness logic; `flock` itself is
    /// the sole source of truth).
    pub fn acquire() -> anyhow::Result<Self> {
        let path = lock_path();
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
            .map_err(|err| anyhow::anyhow!("failed to open lock file {}: {err}", path.display()))?;

        // SAFETY: flock with a valid, open fd and a well-formed operand
        // (LOCK_EX | LOCK_NB) is a well-defined syscall.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                let holder = std::fs::read_to_string(&path).unwrap_or_default();
                let holder = holder.trim();
                anyhow::bail!(
                    "another holoiroh-daemon instance is already running{}; only one instance may run at a time (it owns `holo serve`'s port and the iroh control channel -- a second instance would publish a QR code that silently cannot accept control connections). Stop the other instance first (`kill {}` or close its terminal), then retry.",
                    if holder.is_empty() { String::new() } else { format!(" (pid {holder})") },
                    if holder.is_empty() { "<pid>".to_string() } else { holder.to_string() }
                );
            }
            return Err(anyhow::anyhow!("failed to lock {}: {err}", path.display()));
        }

        // Best-effort PID record for the error message above; the lock
        // itself (not this write) is what actually enforces exclusivity.
        let mut file = file;
        let _ = file.set_len(0);
        let _ = write!(file, "{}", std::process::id());

        Ok(Self { _file: file })
    }
}
