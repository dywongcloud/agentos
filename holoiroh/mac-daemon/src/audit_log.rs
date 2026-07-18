//! Metadata-only local audit log (Project Aro PRD row P0-12).
//!
//! ## What this is
//!
//! A local, append-only, JSON-Lines log at a configurable path (default
//! `~/.holoiroh/audit.log`, resolved the same `$HOME`-join way
//! [`crate::allowlist::Allowlist::default_path`] resolves
//! `~/.holoiroh/allowlist.json`) recording **only**: which task ran, when it
//! started/finished, a coarse app category, a coarse action class, which
//! inference mode served it, whether Remote View was active, whether the
//! connection was direct or relayed, how it ended, how long it took, and how
//! many discrete actions it took.
//!
//! ## Why a typed struct, not a `details: String` field
//!
//! [`AuditEntry`] has **exactly** the ten fields the PRD names -- [`task_id`](AuditEntry::task_id),
//! [`started_at_ms`](AuditEntry::started_at_ms)/[`completed_at_ms`](AuditEntry::completed_at_ms) (the
//! "timings"), [`app_category`](AuditEntry::app_category), [`action_class`](AuditEntry::action_class),
//! [`inference_mode`](AuditEntry::inference_mode), [`remote_view_state`](AuditEntry::remote_view_state),
//! [`connection_path`](AuditEntry::connection_path), [`final_status`](AuditEntry::final_status),
//! [`latency_ms`](AuditEntry::latency_ms), [`action_count`](AuditEntry::action_count) -- and
//! deliberately has **no** catch-all `details: String`/`Value`/`HashMap<String, String>` field of
//! any kind. This is not a style choice: it is what makes it *structurally impossible* for a call
//! site to accidentally log a dictated transcript, a typed prompt, a recipient name, a video frame,
//! a keystroke, or a `holo serve` model prompt/response -- there is no field wide enough to hold any
//! of them. Every field is either a small enum, a `String` restricted by construction to an opaque
//! correlation id ([`task_id`](AuditEntry::task_id), which is `control_channel`'s synthesized
//! `request_id` -- a `uuid::Uuid::new_v4()` value, never user-supplied text), or a plain number.
//! `#[serde(deny_unknown_fields)]` is deliberately **not** used on the way in (this type is
//! serialize-heavy, not a deserializer for untrusted input -- see [`AuditLogger::append`]'s doc), but
//! every field is still individually typed narrowly enough that "pass the wrong thing" is a compile
//! error, not a runtime leak: there is no `String` parameter anywhere in this module's public API
//! that accepts free-form text from a control-channel message.
//!
//! ## Real vs. honestly-approximated fields
//!
//! Three of the ten fields describe daemon/session state this codebase does not yet track with full
//! fidelity as of this writing; each is modeled as a narrow enum with only the variants this daemon
//! can actually distinguish today, documented at its definition, rather than invented:
//!
//! - [`AppCategory`]: this daemon routes every prompt through exactly one downstream agent
//!   (`holo-desktop-cli`, itself capable of driving arbitrary Mac apps) -- there is no per-app
//!   attribution signal anywhere in the control-channel/`holo_bridge` pipeline today, so the only
//!   honest value is [`AppCategory::Desktop`] (the whole-Mac category), not a fabricated per-app
//!   breakdown.
//! - [`InferenceMode`]: `HoloBridge` talks to `holo serve`'s hosted A2A endpoint exclusively (see
//!   `holo_bridge/mod.rs`'s module doc); the on-device/local model path
//!   [`README.md`](../../../README.md)'s Tinfoil/Confidential-Cloud mention describes is a Phase
//!   2/beta item, not built -- so only [`InferenceMode::Cloud`] is ever actually produced today.
//! - [`RemoteViewState`]: the daemon publishes its `iroh-live` broadcast unconditionally before the
//!   control channel is ever mounted (see `main.rs`), so at every point a control-channel connection
//!   can exist, the broadcast is also already live -- there is no code path today where a
//!   control-channel task runs *without* an active broadcast to observe the daemon "starting to
//!   stream". [`RemoteViewState::Streaming`] is therefore the only value this daemon can honestly
//!   report; the variant exists (rather than being collapsed to a bare `bool` hardcoded `true`) so a
//!   future daemon revision that can pause/detach the broadcast independently of the control channel
//!   has a real place to report [`RemoteViewState::Inactive`] without a wire-format change.
//!
//! [`ConnectionPath`] is the one of these three that *is* determined from real, live connection
//! state rather than a fixed default -- see [`ConnectionPath::from_connection`]'s doc for the exact
//! `iroh` API used.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

/// One category of Mac app/surface the task acted on or against.
///
/// See this module's doc comment ("Real vs. honestly-approximated fields") for why only
/// [`Self::Desktop`] is produced by this daemon today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCategory {
    /// The only category this daemon can currently attribute: `holo-desktop-cli` drives the whole
    /// Mac desktop (mouse/keyboard/app control) as a single undifferentiated surface, with no
    /// per-app breakdown surfaced back to this daemon.
    Desktop,
}

/// The coarse kind of control-channel action that started the task.
///
/// Maps directly from [`crate::control_channel::ClientMessage`]'s variants -- this is a
/// classification of *which wire message kind* arrived, never the message's own `text` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Started by a [`crate::control_channel::ClientMessage::Prompt`].
    Prompt,
    /// Started by a [`crate::control_channel::ClientMessage::VoiceTranscript`].
    VoiceTranscript,
    /// Started by a [`crate::control_channel::ClientMessage::Stop`]. Not currently produced by
    /// `control_channel.rs` (a `Stop` message has no `Done`-shaped terminal event of its own to
    /// close an audit entry on -- see `control_channel::audit_on_control_event`'s doc for why
    /// only `Prompt`/`VoiceTranscript` get a start record today), but kept as a real variant
    /// (matching this crate's existing not-yet-called-but-real-API convention, e.g.
    /// `allowlist::Allowlist::remove_entry`) rather than omitted, since a future revision that
    /// audits stop requests as their own task lifecycle would need exactly this variant.
    #[allow(dead_code)]
    Stop,
}

/// Which inference backend served the task.
///
/// See this module's doc comment for why only [`Self::Cloud`] is produced today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMode {
    /// H Company's hosted `holo serve` A2A backend -- the only backend `HoloBridge` talks to as of
    /// this writing (see `holo_bridge/mod.rs`'s module doc).
    Cloud,
    /// Reserved for the not-yet-built on-device/local inference path (Project Aro PRD Phase
    /// 2/beta, Tinfoil/Confidential Cloud). Never produced by this daemon today -- kept as a real
    /// enum variant (not added later as a breaking wire change) so a future local-inference build
    /// can report it without touching every existing log line's shape.
    #[allow(dead_code)]
    Local,
}

/// Whether the `iroh-live` Remote View broadcast was active while the task ran.
///
/// See this module's doc comment for why only [`Self::Streaming`] is produced today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteViewState {
    /// The `iroh-live` broadcast was publishing. The only value this daemon produces today (see
    /// module doc) -- every control-channel connection implies an already-live broadcast.
    Streaming,
    /// Reserved for a future daemon revision that can detach/pause the broadcast independently of
    /// the control channel. Never produced today.
    #[allow(dead_code)]
    Inactive,
}

/// Whether the control-channel connection this task ran over used a direct P2P path or an `iroh`
/// relay fallback (see `holoiroh/README.md`'s "NAT traversal" section for what these mean).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionPath {
    /// Direct QUIC path between the two endpoints (NAT hole-punch succeeded, or same-LAN/no-NAT).
    Direct,
    /// Traffic relayed through an `iroh` relay server (direct connection could not be
    /// established -- see README's "Relay fallback when direct fails").
    Relay,
    /// The connection's currently-selected path could not be determined at the time this was
    /// checked (e.g. no path yet selected -- see [`Self::from_connection`]'s doc). Recorded rather
    /// than silently defaulting to [`Self::Direct`]/[`Self::Relay`], either of which would assert a
    /// specific path this daemon did not actually observe.
    Unknown,
}

impl ConnectionPath {
    /// Determines the connection path from a live `iroh` [`iroh::endpoint::Connection`]'s
    /// currently-selected network path.
    ///
    /// Real `iroh` 1.0.2 API, not guessed: [`iroh::endpoint::Connection::paths`] returns a
    /// [`iroh::endpoint::PathList`] snapshot of the connection's currently-open network paths (per
    /// that method's own doc: "A connection typically has one path via the relay server and, once
    /// holepunching succeeds, a direct path"); each [`iroh::endpoint::connection::Path`] in that
    /// list exposes both `is_selected()` (the path traffic is currently sent over) and `is_relay()`
    /// (delegates to `iroh_base::TransportAddr::is_relay()`). This finds the selected path and maps
    /// it to [`Self::Direct`]/[`Self::Relay`]; [`Self::Unknown`] covers the (normally momentary)
    /// window where no path is yet marked selected.
    pub fn from_connection(connection: &iroh::endpoint::Connection) -> Self {
        match connection.paths().iter().find(|p| p.is_selected()) {
            Some(path) if path.is_relay() => ConnectionPath::Relay,
            Some(_) => ConnectionPath::Direct,
            None => ConnectionPath::Unknown,
        }
    }
}

/// How the task ended.
///
/// Mirrors [`crate::holo_bridge::control::DoneStatus`] (the internal bridge's terminal-state enum)
/// so [`AuditEntry`] never has to depend on `control_channel`/`holo_bridge` internals beyond this
/// one small, already-non-content-bearing enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalStatus {
    Completed,
    Failed,
    Canceled,
}

impl From<crate::holo_bridge::control::DoneStatus> for FinalStatus {
    fn from(status: crate::holo_bridge::control::DoneStatus) -> Self {
        match status {
            crate::holo_bridge::control::DoneStatus::Completed => FinalStatus::Completed,
            crate::holo_bridge::control::DoneStatus::Failed => FinalStatus::Failed,
            crate::holo_bridge::control::DoneStatus::Canceled => FinalStatus::Canceled,
        }
    }
}

/// One append-only audit log record for a single completed control-channel task.
///
/// See this module's doc comment for the full "why exactly these fields, why no catch-all" design
/// rationale. `Serialize`-only (no `Deserialize`): this daemon never needs to parse its own audit
/// log back out of the wire it writes into -- [`examples/audit_log_probe.rs`](../../examples/audit_log_probe.rs)
/// reads it back as a `serde_json::Value` purely to inspect field presence/absence, not to
/// reconstruct an `AuditEntry`.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// Opaque correlation id for this task -- `control_channel`'s synthesized `request_id`
    /// (a `uuid::Uuid::new_v4()` string), never any user-supplied text.
    pub task_id: String,
    /// Unix epoch milliseconds when the task started (the control-channel message that began it
    /// was received and dispatched).
    pub started_at_ms: u64,
    /// Unix epoch milliseconds when the task reached a terminal state.
    pub completed_at_ms: u64,
    pub app_category: AppCategory,
    pub action_class: ActionClass,
    pub inference_mode: InferenceMode,
    pub remote_view_state: RemoteViewState,
    pub connection_path: ConnectionPath,
    pub final_status: FinalStatus,
    /// `completed_at_ms - started_at_ms`. Stored explicitly (not left for a reader to recompute)
    /// so `latency_ms` survives even if a future log format ever drops one of the two timestamps.
    pub latency_ms: u64,
    /// Count of discrete agent actions/progress steps observed for this task (currently: the
    /// number of [`crate::holo_bridge::control::ControlEvent::Progress`] events emitted before the
    /// terminal event -- see `control_channel::ServerMessage::from_control_event`'s mapping from
    /// `ControlEvent` for what a "step" corresponds to on the wire). Never derived from the
    /// content of any step, only their count.
    pub action_count: u32,
}

/// Metadata-only, append-only audit logger.
///
/// Writes one [`AuditEntry`] per line as JSON (JSON Lines / NDJSON, matching
/// `control_channel`'s own newline-delimited wire framing convention) to a file at a configurable
/// path -- default [`AuditLogger::default_path`], `~/.holoiroh/audit.log`.
///
/// ## Concurrency model
///
/// `append` opens the file in append mode (`OpenOptions::append(true)`, i.e. `O_APPEND` on macOS)
/// and writes+flushes synchronously on every call, rather than funneling writes through an
/// `mpsc`-fed background task. This matches `control_channel.rs`'s own documented concurrency
/// model: "this daemon supports exactly one concurrent control-channel connection today" (see
/// `ControlChannel::accept`'s doc comment on `events_tx`), so there is exactly one call site
/// (`ControlChannel::accept`'s per-connection loop, itself single-threaded per connection) that
/// will ever call `append` at a time in practice. `O_APPEND` writes are also atomic at the OS level
/// for writes below the platform pipe/block-size limit (single audit lines are always far under
/// this), so even a hypothetical future second concurrent connection could not interleave partial
/// lines. A background-task/`mpsc` design was considered (see PRD row `audit-logger-append-impl`)
/// and rejected as unneeded complexity for a single-writer daemon; if a future revision adds real
/// multi-connection support, revisit this alongside the same `events_tx`-per-connection redesign
/// `control_channel.rs`'s own doc comment already flags as needed at that point.
#[derive(Debug, Clone)]
pub struct AuditLogger {
    path: PathBuf,
}

impl AuditLogger {
    /// Default location: `~/.holoiroh/audit.log`. Resolved via `$HOME` the same way
    /// [`crate::allowlist::Allowlist::default_path`] resolves `~/.holoiroh/allowlist.json` -- this
    /// daemon is macOS-only, where `$HOME` is always set for an interactive login/launchd session.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home).join(".holoiroh").join("audit.log"))
    }

    /// Resolves the audit log path from the `HOLOIROH_AUDIT_LOG_PATH` environment variable if set,
    /// falling back to [`Self::default_path`] otherwise -- the "configurable path" the PRD names,
    /// following the same env-var-overrides-a-default convention `main.rs`'s `holo_bin()`/
    /// `holo_serve_port()` already use for `HOLOIROH_HOLO_BIN`/`HOLOIROH_HOLO_PORT`.
    pub fn resolve_path() -> Result<PathBuf> {
        match std::env::var_os("HOLOIROH_AUDIT_LOG_PATH") {
            Some(path) => Ok(PathBuf::from(path)),
            None => Self::default_path(),
        }
    }

    /// Constructs a logger writing to `path` directly, without touching env vars or `$HOME` --
    /// the constructor a caller with an already-resolved/overridden path (tests, probes, a future
    /// CLI flag) should use. Creates the parent directory (`~/.holoiroh/` for the default path) if
    /// it doesn't exist yet, matching [`crate::allowlist::Allowlist::save`]'s
    /// `create_dir_all`-on-write pattern -- done eagerly here (at construction) rather than lazily
    /// on first `append`, so a permissions/disk problem is discovered at daemon startup, not
    /// silently on the first real task.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating audit log directory {}", parent.display()))?;
        }
        Ok(Self { path })
    }

    /// Convenience wrapper: [`Self::resolve_path`] then [`Self::new`]. The constructor `main.rs`
    /// calls at daemon startup.
    pub fn from_env() -> Result<Self> {
        Self::new(Self::resolve_path()?)
    }

    /// The path this logger writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `entry` to the log file as one JSON line, opening the file in true append mode
    /// (`OpenOptions::append(true)`, never truncating existing history) and flushing before
    /// returning, so a crash immediately after `append` returns `Ok` cannot lose the line to an
    /// OS-level write buffer.
    ///
    /// Returns `Err` on any I/O failure (serialize failure is not expected -- every [`AuditEntry`]
    /// field is a plain enum/number/opaque-id `String`, none of which can fail to serialize as
    /// JSON) rather than silently dropping the entry; see [`Self::append`]'s callers in
    /// `control_channel.rs` for how a write failure is handled without tearing down the in-flight
    /// control-channel turn that produced it (logged as a warning, not propagated -- matching
    /// `holo_bridge`'s own best-effort/degrade-don't-crash posture).
    pub fn append(&self, entry: &AuditEntry) -> Result<()> {
        let mut line = serde_json::to_string(entry).context("serializing audit log entry")?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening audit log file at {}", self.path.display()))?;
        file.write_all(line.as_bytes())
            .with_context(|| format!("writing audit log entry to {}", self.path.display()))?;
        file.flush()
            .with_context(|| format!("flushing audit log entry to {}", self.path.display()))?;
        Ok(())
    }
}

/// Current Unix epoch time in milliseconds, clamped to `0` on a pre-epoch system clock (matching
/// `allowlist.rs::Allowlist::add_entry`'s own `unwrap_or(0)` fallback for the same
/// `SystemTime::now().duration_since(UNIX_EPOCH)` call, which can only fail if the system clock is
/// set before 1970).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
