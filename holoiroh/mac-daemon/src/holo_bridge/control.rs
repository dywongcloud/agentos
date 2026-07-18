//! Control-channel message types and the bridge that dispatches them to `holo serve`'s A2A
//! endpoint, per the architecture described in `holoiroh/README.md` ("Control channel"
//! section): text prompts and voice transcripts flow *into* the daemon from the iOS app,
//! status/log/ack events flow back *out*.
//!
//! ## What is and isn't defined elsewhere
//!
//! The wire format of the control channel itself (how messages are framed over the `iroh`
//! control stream) is defined separately in `crate::control_channel` and
//! `holoiroh/PROTOCOL.md` -- a minimal `ClientMessage`/`ServerMessage` schema
//! (`{type, text?}`, no correlation ids) carried over a dedicated ALPN on the same `iroh`
//! `Endpoint` as the media broadcast. This module's `ControlMessage` / `ControlEvent` are a
//! *richer, internal* schema (`serde`-tagged enums, transport-agnostic) correlated by
//! `request_id`/`context_id` for talking to `holo serve`'s A2A endpoint; `control_channel`
//! translates between the two at the transport boundary (see its module doc for the mapping)
//! rather than this module depending on `iroh` or wire framing at all. `prompt` /
//! `voice_transcript` / `stop` are exactly the three message kinds named in the task;
//! `voice_transcript` is modeled identically to `prompt` (both become an A2A `message/stream`
//! call with the transcript/prompt text as the message body) since `README.md` §"iOS-side" is
//! explicit that "voice input is transcribed ... before being sent as text over the control
//! channel, so the wire format is always a text prompt plus metadata, never raw audio" -- i.e.
//! by the time either message kind reaches this bridge, both are just text.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};
use tokio::sync::mpsc;

use crate::holo_bridge::a2a_client::{A2aClient, TaskUpdate, TerminalState};

/// One incoming control-channel message, keyed by the `type` discriminator the task
/// description names explicitly (`"prompt"`, `"voice_transcript"`, `"stop"`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// A typed text prompt from the iOS app's text field.
    Prompt {
        /// Free-form id the caller can use to correlate this prompt with the
        /// `ControlEvent`s it produces; echoed back on every event derived from it.
        request_id: String,
        text: String,
        /// Continue an existing A2A/`hai-agent-runtime` session (see `a2a_client` module doc
        /// on `contextId`), or start a new one when absent.
        #[serde(default)]
        context_id: Option<String>,
    },
    /// A transcribed voice instruction. Same handling as `Prompt` (see module doc) -- kept as
    /// a distinct variant rather than collapsed into `Prompt` so the iOS app / future control
    /// channel can still distinguish "typed" vs. "spoken" provenance for UI/logging purposes,
    /// without that distinction leaking into how the daemon talks to `holo serve`.
    VoiceTranscript {
        request_id: String,
        text: String,
        #[serde(default)]
        context_id: Option<String>,
        /// Optional transcription confidence, if the on-device/service transcriber supplies
        /// one; purely informational, never gates whether the daemon acts on it.
        #[serde(default)]
        confidence: Option<f32>,
    },
    /// Stop the in-flight turn. `context_id` scopes the stop to one A2A task via the A2A
    /// `tasks/cancel` equivalent; when absent, this also engages the CLI-level `holo stop`
    /// kill switch (global to the `holo serve`-spawned runtime, matching the double-Esc /
    /// `holo stop` behavior documented in the upstream CLI -- see `stop.rs` module doc).
    Stop {
        request_id: String,
        #[serde(default)]
        context_id: Option<String>,
        /// Mirrors `holo stop --force`: after requesting the graceful pause-then-cancel,
        /// also SIGKILL the underlying `hai-agent-runtime` process. Use only when the
        /// graceful path is known to be stuck; it ends every in-flight session, not just
        /// this `context_id`.
        #[serde(default)]
        force: bool,
    },
}

impl ControlMessage {
    /// Not read internally today (`HoloControlBridge::handle` destructures each variant
    /// itself), but a natural accessor for any future caller that wants to log/correlate a
    /// message before dispatching it -- kept rather than removed just to silence dead-code.
    #[allow(dead_code)]
    pub fn request_id(&self) -> &str {
        match self {
            ControlMessage::Prompt { request_id, .. }
            | ControlMessage::VoiceTranscript { request_id, .. }
            | ControlMessage::Stop { request_id, .. } => request_id,
        }
    }
}

/// One outgoing control-channel event, reporting progress/status back to the iOS app for a
/// given `request_id`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// Acknowledges receipt before any A2A round trip completes, so the UI can show
    /// "sent" immediately rather than waiting on the first task-progress event.
    Ack { request_id: String },
    /// One step of agent progress. `raw_event`, when present, is the backend
    /// `TrajectoryEvent` forwarded verbatim (opaque JSON -- see `a2a_client` module doc on
    /// why this bridge does not attempt to type it); `text`, when present, is a
    /// human-readable status line extracted from the A2A status message.
    Progress {
        request_id: String,
        context_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_event: Option<Value>,
    },
    /// Final answer text for the turn.
    Answer {
        request_id: String,
        context_id: Option<String>,
        text: String,
    },
    /// Terminal status: completed, failed, or canceled.
    Done {
        request_id: String,
        context_id: Option<String>,
        status: DoneStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// The bridge itself hit an error before/without a well-formed A2A terminal event (e.g.
    /// `holo serve` unreachable, malformed control message).
    Error { request_id: String, message: String },
    /// A `Prompt`/`VoiceTranscript` arrived while a previous turn was still in flight and was
    /// queued rather than raced against it or dropped. `ahead` is the number of prompts already
    /// queued in front of this one (0 means "runs as soon as the current turn finishes").
    /// `crate::control_channel::ServerMessage::from_control_event` maps this to the wire
    /// `{"type":"status","text":"queued, N ahead"}` the task asks for.
    Queued { request_id: String, ahead: usize },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneStatus {
    Completed,
    Failed,
    Canceled,
}

impl From<TerminalState> for DoneStatus {
    fn from(state: TerminalState) -> Self {
        match state {
            TerminalState::Completed => DoneStatus::Completed,
            TerminalState::Failed => DoneStatus::Failed,
            TerminalState::Canceled => DoneStatus::Canceled,
        }
    }
}

/// One `Prompt`/`VoiceTranscript` turn waiting for the in-flight turn ahead of it to finish.
/// Both `ControlMessage` variants that carry free-form text collapse to this one shape once
/// queued -- the queue only needs to replay `send_and_stream`'s inputs, not the original
/// wire-level distinction (which was already informational-only, see `ControlMessage` doc).
struct QueuedPrompt {
    request_id: String,
    text: String,
    context_id: Option<String>,
}

/// Bridges control-channel messages to `holo serve`'s A2A endpoint and CLI-level stop
/// handling. Holds no transport of its own -- the caller owns receiving `ControlMessage`s
/// (from whatever the eventual `iroh` control stream deserializes into them) and sending
/// `ControlEvent`s back out; this type only owns the A2A/CLI interaction and the
/// prompt-to-context continuity.
pub struct HoloControlBridge {
    client: A2aClient,
    /// Wrapped in a `std::sync::RwLock` (not `tokio::sync::RwLock`) so the
    /// active event sink can be swapped per control-channel connection
    /// (see `crate::control_channel`, which calls
    /// `HoloBridge::replace_event_sink` once per accepted connection)
    /// while `emit` stays synchronous -- `emit` is called from inside the
    /// synchronous `FnMut(TaskUpdate)` callback `A2aClient::send_and_stream`
    /// takes, so it cannot `.await`. A std lock is safe here because both
    /// the read (clone a `Sender`, itself a cheap non-blocking op) and the
    /// write (swap a `Sender`) critical sections are tiny and never hold
    /// the lock across an `.await` point. This is also exactly the
    /// mechanism a reconnect relies on: a brand-new control-channel
    /// connection calls `replace_event_sink` on accept, so any turn that
    /// was already streaming (from a prompt submitted on a now-dropped
    /// connection, or drained from the queue below) has its *next* `emit`
    /// routed to the newly-connected peer -- no daemon restart, no lost
    /// turn, just a redirected sink.
    events_tx: RwLock<mpsc::UnboundedSender<ControlEvent>>,
    /// Path to (or bare name of) the `holo` CLI binary, for `holo stop` (see `stop.rs`).
    holo_bin: String,
    /// `true` while a `Prompt`/`VoiceTranscript` turn is actively running against `client`
    /// (i.e. inside `send_and_stream`). Guards against ever having two simultaneous
    /// `send_and_stream` calls in flight against the same `holo serve`/`AgentApiClient`
    /// session -- see `run_or_queue_prompt`. A `std::sync::Mutex<bool>` rather than an
    /// `AtomicBool` so the "check busy, and if free mark busy" step is one atomic
    /// critical section together with the queue-length check below (a plain
    /// compare-and-swap on its own can't also observe/mutate `queue` in the same step).
    busy: Mutex<bool>,
    /// Prompts that arrived while `busy` was `true`, oldest-first (`pop_front` drains in
    /// arrival order). Guarded by the same lock discipline as `busy`: both are read/written
    /// together under `queue`'s own mutex so "is anything running" and "what's queued" never
    /// observe a torn state relative to each other.
    queue: Mutex<VecDeque<QueuedPrompt>>,
}

impl HoloControlBridge {
    pub fn new(
        client: A2aClient,
        holo_bin: impl Into<String>,
        events_tx: mpsc::UnboundedSender<ControlEvent>,
    ) -> Self {
        Self {
            client,
            events_tx: RwLock::new(events_tx),
            holo_bin: holo_bin.into(),
            busy: Mutex::new(false),
            queue: Mutex::new(VecDeque::new()),
        }
    }

    /// Swaps the sink that [`emit`](Self::emit) sends
    /// [`ControlEvent`]s to. Used when a new control-channel connection is
    /// accepted (see `crate::control_channel::ControlChannel::accept`) so
    /// events from this bridge's in-flight/future turns are routed to the
    /// currently-connected peer rather than a stale, possibly-closed sink
    /// from a previous connection.
    pub fn replace_event_sink(&self, events_tx: mpsc::UnboundedSender<ControlEvent>) {
        *self.events_tx.write().expect("events_tx lock poisoned") = events_tx;
    }

    fn emit(&self, event: ControlEvent) {
        // A dropped receiver (control channel torn down mid-turn) is not this bridge's
        // problem to escalate -- the in-flight A2A call keeps running to completion
        // server-side regardless, matching holo-desktop-cli's own "cancel is best-effort,
        // local state always resets" stance in session_runner.py.
        let _ = self
            .events_tx
            .read()
            .expect("events_tx lock poisoned")
            .send(event);
    }

    /// Handle one incoming control message end-to-end. Prompts/transcripts stream until the
    /// A2A task reaches a terminal state (or the bridge errors); stop requests return as soon
    /// as the stop/cancel calls are issued (mirrors `holo stop`'s own fire-and-forget
    /// semantics -- see `stop.rs`).
    pub async fn handle(&self, message: ControlMessage) {
        match message {
            ControlMessage::Prompt {
                request_id,
                text,
                context_id,
            }
            | ControlMessage::VoiceTranscript {
                request_id,
                text,
                context_id,
                confidence: _,
            } => {
                self.handle_prompt(request_id, text, context_id).await;
            }
            ControlMessage::Stop {
                request_id,
                context_id,
                force,
            } => {
                self.handle_stop(request_id, context_id, force).await;
            }
        }
    }

    /// Entry point for `Prompt`/`VoiceTranscript`: either runs the turn immediately (no turn
    /// currently in flight) or queues it and returns, per the task's explicit "queue, don't
    /// drop, don't race" requirement. `context_id` is always `None` on the way in today (see
    /// `to_control_message`'s doc), so every queued prompt starts a fresh A2A context in the
    /// order it was received -- exactly the sequencing a single-active-session
    /// `AgentApiClient` requires.
    async fn handle_prompt(&self, request_id: String, text: String, context_id: Option<String>) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        {
            // Single critical section: observe-and-flip `busy` and (if already busy)
            // push onto `queue` atomically, so a second `handle_prompt` call racing in
            // from a different accepted connection can never both see `busy == false`
            // and both proceed to `run_prompt` -- exactly the "two simultaneous
            // requests" race the task calls out.
            let mut busy = self.busy.lock().expect("busy lock poisoned");
            if *busy {
                let mut queue = self.queue.lock().expect("queue lock poisoned");
                let ahead = queue.len();
                queue.push_back(QueuedPrompt {
                    request_id: request_id.clone(),
                    text,
                    context_id,
                });
                drop(queue);
                drop(busy);
                tracing::debug!(request_id, ahead, "prompt queued behind an in-flight turn");
                self.emit(ControlEvent::Queued { request_id, ahead });
                return;
            }
            *busy = true;
        }

        self.run_prompt(request_id, text, context_id.as_deref())
            .await;
        self.drain_queue().await;
    }

    /// Runs one turn against `client` end-to-end (streaming progress, terminal
    /// ack/answer/error). Does not touch `busy`/`queue` itself -- callers
    /// (`handle_prompt` for the immediately-run case, `drain_queue` for queued ones) own the
    /// busy-flag lifecycle so this stays a plain "run this one turn" primitive reusable from
    /// both call sites.
    async fn run_prompt(&self, request_id: String, text: String, context_id: Option<&str>) {
        let request_id_for_updates = request_id.clone();
        let result = self
            .client
            .send_and_stream(&text, context_id, |update| {
                self.emit(translate_update(&request_id_for_updates, update));
            })
            .await;

        match result {
            Ok(resolved_context_id) => {
                tracing::debug!(
                    request_id,
                    context_id = %resolved_context_id,
                    "prompt turn finished"
                );
            }
            Err(err) => {
                tracing::warn!(request_id, error = %err, "prompt turn failed before a terminal A2A state");
                self.emit(ControlEvent::Error {
                    request_id,
                    message: err.to_string(),
                });
            }
        }
    }

    /// After a turn finishes, pop and run the next queued prompt (if any), looping until the
    /// queue is empty or a `Stop` (running concurrently under the same locks) drains it out
    /// from under this loop. Re-emits `Queued { ahead: 0 }`-implying progress by simply
    /// starting the next turn's own `Ack`/`Progress`/... events -- the queued prompts behind
    /// it, if any, keep whatever `ahead` count they were given when they were first queued;
    /// re-announcing a shrinking count on every drain step is not required by the task's
    /// acceptance (one queued-status message per queued prompt) and would just be additional
    /// noise on every drain step for long queues.
    async fn drain_queue(&self) {
        loop {
            let next = {
                let mut queue = self.queue.lock().expect("queue lock poisoned");
                let next = queue.pop_front();
                let mut busy = self.busy.lock().expect("busy lock poisoned");
                *busy = next.is_some();
                next
            };
            let Some(QueuedPrompt {
                request_id,
                text,
                context_id,
            }) = next
            else {
                break;
            };
            self.run_prompt(request_id, text, context_id.as_deref())
                .await;
        }
    }

    async fn handle_stop(&self, request_id: String, context_id: Option<String>, force: bool) {
        self.emit(ControlEvent::Ack {
            request_id: request_id.clone(),
        });

        // `holo stop` (below) only reaches the *currently running* turn -- it polls
        // `~/.holo/stop` from inside `session_runner.run_turn`, which a still-queued prompt
        // has not entered yet (see stop.rs's module doc). Left alone, queued prompts would
        // survive a Stop and get dispatched to `holo serve` right after, silently ignoring
        // the user's stop. Drain the queue here and give each dropped prompt its own terminal
        // `Done{Canceled}` event instead of leaving its `request_id` unresolved forever on the
        // iOS side.
        let dropped: Vec<QueuedPrompt> = {
            let mut queue = self.queue.lock().expect("queue lock poisoned");
            queue.drain(..).collect()
        };
        for dropped_prompt in dropped {
            tracing::debug!(
                request_id = dropped_prompt.request_id,
                "queued prompt dropped by stop"
            );
            self.emit(ControlEvent::Done {
                request_id: dropped_prompt.request_id,
                context_id: dropped_prompt.context_id,
                status: DoneStatus::Canceled,
                message: Some("canceled: stop requested while queued".to_owned()),
            });
        }

        // Scoped cancel first, when we have a context to scope it to: the A2A
        // `tasks/cancel`-equivalent path (HoloExecutor.cancel -> best-effort backend
        // session cancel; see a2a_client module doc). This is the lower-blast-radius option
        // and should win when both are meaningful.
        if let Some(ctx) = context_id.as_deref() {
            if let Err(err) = self.client.cancel(ctx, None).await {
                tracing::warn!(request_id, context_id = ctx, error = %err, "A2A tasks/cancel failed");
                self.emit(ControlEvent::Error {
                    request_id: request_id.clone(),
                    message: format!("A2A cancel failed for context {ctx}: {err}"),
                });
            }
        }

        // Global `holo stop` kill switch: always issued when no context_id was given (the
        // caller wants "stop whatever is running"), or when `force` was requested (force
        // implies a process-level SIGKILL that only the CLI path performs -- see stop.rs).
        if context_id.is_none() || force {
            if let Err(err) = crate::holo_bridge::stop::holo_stop(&self.holo_bin, force).await {
                tracing::warn!(request_id, error = %err, "holo stop failed");
                self.emit(ControlEvent::Error {
                    request_id: request_id.clone(),
                    message: format!("holo stop failed: {err}"),
                });
                return;
            }
        }

        self.emit(ControlEvent::Done {
            request_id,
            context_id,
            status: DoneStatus::Canceled,
            message: Some(if force {
                "stop requested (forced)".to_owned()
            } else {
                "stop requested".to_owned()
            }),
        });
    }
}

fn translate_update(request_id: &str, update: TaskUpdate) -> ControlEvent {
    match update {
        TaskUpdate::Working { raw_event, text } => ControlEvent::Progress {
            request_id: request_id.to_owned(),
            context_id: None,
            text,
            raw_event,
        },
        TaskUpdate::Answer { text } => ControlEvent::Answer {
            request_id: request_id.to_owned(),
            context_id: None,
            text,
        },
        TaskUpdate::Terminal { state, message } => ControlEvent::Done {
            request_id: request_id.to_owned(),
            context_id: None,
            status: state.into(),
            message,
        },
    }
}
