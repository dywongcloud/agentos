//! Metadata-only local audit log (Project Aro PRD row P0-12).
//!
//! ## What this is
//!
//! A local, append-only, JSON-Lines log records task metadata at a
//! configurable path. The default path is `~/.holoiroh/audit.log`.
//! [`crate::allowlist::Allowlist::default_path`] resolves
//! `~/.holoiroh/allowlist.json` the same way, through a `$HOME` join.
//!
//! The log records **only**:
//!
//! - which task ran
//! - when the task started and finished
//! - a coarse app category
//! - a coarse action class
//! - which inference mode served the task
//! - whether Remote View was active
//! - whether the connection was direct or relayed
//! - how the task ended
//! - how long the task took
//! - how many discrete actions the task took
//!
//! ## Why a typed struct, not a `details: String` field
//!
//! [`AuditEntry`] has **exactly** the ten fields the PRD names:
//!
//! - [`task_id`](AuditEntry::task_id)
//! - [`started_at_ms`](AuditEntry::started_at_ms) and
//!   [`completed_at_ms`](AuditEntry::completed_at_ms) (the "timings")
//! - [`app_category`](AuditEntry::app_category)
//! - [`action_class`](AuditEntry::action_class)
//! - [`inference_mode`](AuditEntry::inference_mode)
//! - [`remote_view_state`](AuditEntry::remote_view_state)
//! - [`connection_path`](AuditEntry::connection_path)
//! - [`final_status`](AuditEntry::final_status)
//! - [`latency_ms`](AuditEntry::latency_ms)
//! - [`action_count`](AuditEntry::action_count)
//!
//! [`AuditEntry`] deliberately has **no** catch-all `details: String`/`Value`/
//! `HashMap<String, String>` field of any kind. This is not a style choice.
//! This design makes it *structurally impossible* for a call site to log any
//! of the following by accident:
//!
//! - a dictated transcript
//! - a typed prompt
//! - a recipient name
//! - a video frame
//! - a keystroke
//! - a `holo serve` model prompt/response
//!
//! No field is wide enough to hold any of them.
//!
//! Every field is one of three kinds: a small enum, a `String`, or a plain
//! number. Every `String` field is restricted by construction to an opaque
//! correlation id. For example, [`task_id`](AuditEntry::task_id) is
//! `control_channel`'s synthesized `request_id` -- a `uuid::Uuid::new_v4()`
//! value, never user-supplied text.
//!
//! `#[serde(deny_unknown_fields)]` is deliberately **not** used on the way
//! in. This type is serialize-heavy, not a deserializer for untrusted input
//! -- see [`AuditLogger::append`]'s doc. Every field is still typed narrowly
//! enough that passing "the wrong thing" is a compile error, not a runtime
//! leak. No `String` parameter anywhere in this module's public API accepts
//! free-form text from a control-channel message.
//!
//! ## Real vs. honestly-approximated fields
//!
//! Three of the ten fields describe daemon or session state that this
//! codebase does not yet track with full fidelity, as of this writing.
//! Each of the three is modeled as a narrow enum, with only the variants
//! this daemon can actually distinguish today. Each is documented at its
//! own definition, rather than invented:
//!
//! - [`AppCategory`]: this daemon routes every prompt through exactly one
//!   downstream agent, `holo-desktop-cli`. `holo-desktop-cli` can drive
//!   arbitrary Mac apps. The control-channel/`holo_bridge` pipeline has no
//!   per-app attribution signal today. The only honest value is therefore
//!   [`AppCategory::Desktop`], the whole-Mac category, not a fabricated
//!   per-app breakdown.
//! - [`InferenceMode`]: `HoloBridge` talks only to `holo serve`'s hosted A2A
//!   endpoint (see `holo_bridge/mod.rs`'s module doc). The on-device/local
//!   model path that [`README.md`](../../../README.md) describes under
//!   Tinfoil/Confidential-Cloud is a Phase 2/beta item, not yet built. Only
//!   [`InferenceMode::Cloud`] is ever actually produced today.
//! - [`RemoteViewState`]: the daemon publishes its `iroh-live` broadcast
//!   unconditionally, before the control channel is ever mounted (see
//!   `main.rs`). At every point a control-channel connection can exist, the
//!   broadcast is also already live. No code path today runs a
//!   control-channel task *without* an active broadcast to observe the
//!   daemon "starting to stream". [`RemoteViewState::Streaming`] is
//!   therefore the only value this daemon can honestly report. The variant
//!   exists instead of being collapsed to a bare `bool` hardcoded to `true`.
//!   A future daemon revision can pause or detach the broadcast
//!   independently of the control channel. Such a revision can then report
//!   [`RemoteViewState::Inactive`] without a wire-format change.
//!
//! [`ConnectionPath`] is the one of these three that *is* determined from
//! real, live connection state, rather than from a fixed default. See
//! [`ConnectionPath::from_connection`]'s doc for the exact `iroh` API used.

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
    /// The only category this daemon can currently attribute: `holo-desktop-cli`
    /// drives the whole Mac desktop (mouse/keyboard/app control) as a single
    /// undifferentiated surface. `holo-desktop-cli` does not surface any per-app
    /// breakdown back to this daemon.
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
    /// Started by a [`crate::control_channel::ClientMessage::Stop`]. `control_channel.rs`
    /// does not produce this variant today. A `Stop` message has no
    /// `Done`-shaped terminal event of its own to close an audit entry on. See
    /// `control_channel::audit_on_control_event`'s doc for why only
    /// `Prompt`/`VoiceTranscript` get a start record today. This crate keeps
    /// `Stop` as a real variant rather than omitting it, matching this crate's
    /// existing not-yet-called-but-real-API convention (for example,
    /// `allowlist::Allowlist::remove_entry`). A future revision that audits stop
    /// requests as their own task lifecycle needs exactly this variant.
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
    /// This variant is reserved for the not-yet-built on-device/local inference
    /// path (Project Aro PRD Phase 2/beta, Tinfoil/Confidential Cloud). This
    /// daemon never produces this variant today. This crate keeps it as a real
    /// enum variant, not added later as a breaking wire change. A future
    /// local-inference build can report it without touching every existing log
    /// line's shape.
    #[allow(dead_code)]
    Local,
}

/// Whether the `iroh-live` Remote View broadcast was active while the task ran.
///
/// See this module's doc comment for why only [`Self::Streaming`] is produced today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteViewState {
    /// The `iroh-live` broadcast was publishing. This is the only value this
    /// daemon produces today (see module doc). Every control-channel connection
    /// implies an already-live broadcast.
    Streaming,
    /// Reserved for a future daemon revision that can detach/pause the broadcast independently of
    /// the control channel. Never produced today.
    #[allow(dead_code)]
    Inactive,
}

/// Whether the control-channel connection this task ran over used a direct
/// P2P path or an `iroh` relay fallback. See `holoiroh/README.md`'s "NAT
/// traversal" section for what these mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionPath {
    /// Direct QUIC path between the two endpoints (NAT hole-punch succeeded, or same-LAN/no-NAT).
    Direct,
    /// Traffic relayed through an `iroh` relay server (direct connection could not be
    /// established -- see README's "Relay fallback when direct fails").
    Relay,
    /// At the time of the check, this daemon did not identify the connection's
    /// currently-selected path -- for example, no path was yet selected (see
    /// [`Self::from_connection`]'s doc). This daemon records [`Self::Unknown`]
    /// in that case, instead of silently defaulting to [`Self::Direct`] or
    /// [`Self::Relay`]. A silent default to either value asserts a specific
    /// path. This daemon did not actually observe that path.
    Unknown,
}

impl ConnectionPath {
    /// Determines the connection path from a live `iroh` [`iroh::endpoint::Connection`]'s
    /// currently-selected network path.
    ///
    /// Real `iroh` 1.0.2 API, not guessed. [`iroh::endpoint::Connection::paths`]
    /// returns a [`iroh::endpoint::PathList`] snapshot of the connection's
    /// currently-open network paths. Per that method's own doc: "A connection
    /// typically has one path via the relay server and, once holepunching
    /// succeeds, a direct path."
    ///
    /// Each [`iroh::endpoint::connection::Path`] in that list exposes two
    /// methods:
    ///
    /// - `is_selected()`: the path traffic is currently sent over
    /// - `is_relay()`: delegates to `iroh_base::TransportAddr::is_relay()`
    ///
    /// This function finds the selected path and maps it to [`Self::Direct`] or
    /// [`Self::Relay`]. [`Self::Unknown`] covers the window, normally
    /// momentary, where no path is yet marked selected.
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
/// See this module's doc comment for the full "why exactly these fields,
/// why no catch-all" design rationale. This type is `Serialize`-only. It has
/// no `Deserialize`. This daemon never needs to parse its own audit log
/// back out of the wire it writes into.
/// [`examples/audit_log_probe.rs`](../../examples/audit_log_probe.rs) reads
/// the log back as a `serde_json::Value`, purely to inspect field
/// presence/absence. That probe does not reconstruct an `AuditEntry`.
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

/// Which Tinfoil cloud-egress capability a [`CloudEgressEntry`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudEgressCapability {
    Document,
    Image,
    AudioTranscribe,
    AudioSpeech,
    Planner,
}

/// One record of data leaving the device to Tinfoil's confidential-computing cloud
/// (`tinfoil_documents`/`tinfoil_vision`/`tinfoil_audio`/`tinfoil_planner`).
///
/// A **sibling** to [`AuditEntry`], not a repurposing of it. `AuditEntry` is
/// a closed 10-field schema, purpose-built for `holo-desktop-cli` task
/// lifecycle (see this module's doc on why it has no catch-all field). None
/// of its fields -- `app_category`, `inference_mode`, `remote_view_state`,
/// `action_count`, and so on -- have a meaningful value for "a document was
/// uploaded". Forcing this event shape into `AuditEntry` means inventing
/// fake values for fields that don't apply. That is the exact "no
/// fabricated per-app breakdown" failure mode this module's own doc already
/// rejects for `AppCategory`.
///
/// This type follows the identical design discipline instead: exactly the
/// fields that describe *what left the device and whether it succeeded*,
/// with deliberately no `details`/`text`/`Value` catch-all. Because of this
/// design, it is structurally impossible for a call site to log a
/// document's content, an image, a transcript, or a plan's text here.
#[derive(Debug, Clone, Serialize)]
pub struct CloudEgressEntry {
    /// Opaque correlation id -- the wire `request_id` the client supplied, never user-supplied
    /// free text (client-generated ids only correlate; they carry no semantic content).
    pub request_id: String,
    pub capability: CloudEgressCapability,
    /// Unix epoch milliseconds when the request completed (success or failure).
    pub occurred_at_ms: u64,
    pub success: bool,
    /// Size of the data sent off-device (file bytes / image bytes / audio bytes / goal text
    /// length), never the data itself.
    pub byte_count: u64,
}

/// Metadata-only, append-only audit logger.
///
/// Writes one [`AuditEntry`] per line as JSON (JSON Lines / NDJSON). This
/// format matches `control_channel`'s own newline-delimited wire framing
/// convention. The logger writes to a file at a configurable path -- the
/// default is [`AuditLogger::default_path`], `~/.holoiroh/audit.log`.
///
/// ## Concurrency model
///
/// `append` opens the file in append mode: `OpenOptions::append(true)`, i.e.
/// `O_APPEND` on macOS. `append` writes and flushes synchronously on every
/// call, instead of funneling writes through an `mpsc`-fed background task.
/// This matches `control_channel.rs`'s own documented concurrency model:
/// "this daemon supports exactly one concurrent control-channel connection
/// today" (see `ControlChannel::accept`'s doc comment on `events_tx`).
/// Because of that model, exactly one call site --
/// `ControlChannel::accept`'s per-connection loop, itself single-threaded
/// per connection -- ever calls `append` at a time in practice.
///
/// `O_APPEND` writes are also atomic at the OS level, for writes below the
/// platform pipe/block-size limit. Single audit lines are always far under
/// this limit. So even a hypothetical future second concurrent connection
/// cannot interleave partial lines.
///
/// This module's design rejects a background-task/`mpsc` alternative (see
/// PRD row `audit-logger-append-impl`) as unneeded complexity for a
/// single-writer daemon. If a future revision adds real multi-connection
/// support, revisit this decision alongside the same
/// `events_tx`-per-connection redesign that `control_channel.rs`'s own doc
/// comment already flags as needed at that point.
#[derive(Debug, Clone)]
pub struct AuditLogger {
    path: PathBuf,
}

impl AuditLogger {
    /// Default location: `~/.holoiroh/audit.log`. This path is resolved via
    /// `$HOME`, the same way [`crate::allowlist::Allowlist::default_path`]
    /// resolves `~/.holoiroh/allowlist.json`. This daemon is macOS-only, where
    /// `$HOME` is always set for an interactive login or launchd session.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home).join(".holoiroh").join("audit.log"))
    }

    /// Resolves the audit log path from the `HOLOIROH_AUDIT_LOG_PATH`
    /// environment variable, if that variable is set. Falls back to
    /// [`Self::default_path`] otherwise. This is the "configurable path" the
    /// PRD names. It follows the same env-var-overrides-a-default convention
    /// that `main.rs`'s `holo_bin()`/`holo_serve_port()` already use for
    /// `HOLOIROH_HOLO_BIN`/`HOLOIROH_HOLO_PORT`.
    pub fn resolve_path() -> Result<PathBuf> {
        match std::env::var_os("HOLOIROH_AUDIT_LOG_PATH") {
            Some(path) => Ok(PathBuf::from(path)),
            None => Self::default_path(),
        }
    }

    /// Constructs a logger that writes to `path` directly, without touching env
    /// vars or `$HOME`. A caller with an already-resolved or overridden path --
    /// tests, probes, or a future CLI flag -- should use this constructor.
    ///
    /// This constructor creates the parent directory (`~/.holoiroh/` for the
    /// default path) if it doesn't exist yet. This matches
    /// [`crate::allowlist::Allowlist::save`]'s `create_dir_all`-on-write
    /// pattern. It creates the directory eagerly, at construction, rather than
    /// lazily on first `append`. This way, a permissions or disk problem is
    /// discovered at daemon startup, not silently on the first real task.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating audit log directory {}", parent.display()))?;
        }
        Ok(Self { path })
    }

    /// Convenience wrapper: calls [`Self::resolve_path`] then [`Self::new`].
    /// This is the constructor that `main.rs` calls at daemon startup.
    pub fn from_env() -> Result<Self> {
        Self::new(Self::resolve_path()?)
    }

    /// The path this logger writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `entry` to the log file as one JSON line. This method opens the
    /// file in true append mode: `OpenOptions::append(true)`, which never
    /// truncates existing history. This method flushes the file before
    /// returning. Because of the flush, a crash immediately after `append`
    /// returns `Ok` cannot lose the line to an OS-level write buffer.
    ///
    /// Returns `Err` on any I/O failure, rather than silently dropping the
    /// entry. A serialize failure is not expected. Every [`AuditEntry`] field
    /// is a plain enum, number, or opaque-id `String`. None of these types can
    /// fail to serialize as JSON. See [`Self::append`]'s callers in
    /// `control_channel.rs` for how a write failure is handled without tearing
    /// down the in-flight control-channel turn that produced it. A write
    /// failure is logged as a warning, not propagated, matching
    /// `holo_bridge`'s own best-effort/degrade-don't-crash posture.
    ///
    /// This method is generic over `T: Serialize`. Because of this, both
    /// [`AuditEntry`] (task lifecycle) and [`CloudEgressEntry`] (Tinfoil
    /// cloud-egress) share this one write path. The method body was never
    /// actually specific to `AuditEntry`'s shape -- it only serializes,
    /// appends, and flushes. Widening the bound is therefore a pure
    /// generalization, not a behavior change for existing `AuditEntry` call
    /// sites.
    pub fn append<T: Serialize>(&self, entry: &T) -> Result<()> {
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

/// Current Unix epoch time in milliseconds, clamped to `0` on a pre-epoch
/// system clock. This matches `allowlist.rs::Allowlist::add_entry`'s own
/// `unwrap_or(0)` fallback for the same
/// `SystemTime::now().duration_since(UNIX_EPOCH)` call. That call can only
/// fail if the system clock is set before 1970.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
