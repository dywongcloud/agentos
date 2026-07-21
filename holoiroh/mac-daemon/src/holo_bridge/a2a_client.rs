//! Minimal A2A (Agent2Agent Protocol, <https://a2a-protocol.org>) JSON-RPC client scoped to
//! exactly what this daemon needs from `holo serve`: submit a prompt and stream back
//! task/status events, and cancel a task.
//!
//! ## Source grounding -- what is confirmed vs. what is spec-inferred
//!
//! **Confirmed directly from `hcompai/holo-desktop-cli` source**
//! (`src/holo_desktop/cli/serve.py`, read via the GitHub API on 2026-07-17):
//!
//! - `holo serve` mounts a standard `a2a-sdk` (`a2a-sdk[http-server]>=1.0.3` per
//!   `pyproject.toml`) server: `create_agent_card_routes(card)` for agent-card discovery and
//!   `create_jsonrpc_routes(handler, "/a2a", enable_v0_3_compat=True)` for the RPC endpoint
//!   itself, so JSON-RPC requests go to `POST {base_url}/a2a`.
//! - The published `AgentCard` declares
//!   `capabilities=AgentCapabilities(streaming=True, push_notifications=False)` and
//!   `supported_interfaces=[AgentInterface(url=..., protocol_binding="JSONRPC",
//!   protocol_version="0.3.0")]`.
//! - Every request except `GET /health` requires `Authorization: Bearer <token>`
//!   (`BearerAuthMiddleware`), checked with `hmac.compare_digest` against the token `holo
//!   serve` was started with.
//! - One A2A **`contextId`** maps 1:1 to one `hai-agent-runtime` agent-API session
//!   (`HoloExecutor._sessions: OrderedDict[str, Session]`, keyed by a sanitized `contextId`).
//!   Reusing the same `contextId` across calls continues the same conversation/session;
//!   a new/absent `contextId` starts a fresh one. Sessions are capped at 256 concurrent
//!   (`MAX_RETAINED_SESSIONS`), LRU-evicted (with best-effort cancel of the evicted session)
//!   once the cap is hit.
//! - Streamed progress: for each backend `TrajectoryEvent`, the executor emits one A2A
//!   `TaskStatusUpdateEvent` with `status.state = TASK_STATE_WORKING` and
//!   `status.message` = a **data message** (`a2a.helpers.new_data_message`) whose payload is
//!   the raw `TrajectoryEvent`, serialized as JSON, under
//!   media type `application/vnd.holo-desktop.event+json`
//!   (`translate_event()` / `EVENT_MEDIA_TYPE` in `serve.py`). The exact shape of a
//!   `TrajectoryEvent` (fields like `type`, `data`, event `kind` discriminators such as
//!   `PolicyEvent`/`ToolResultEvent`/`AnswerEvent`/`ErrorEvent`) lives in the closed-source
//!   `agp_types`/`agent_interface` packages (`hai-agent-api` on PyPI) and was **not**
//!   independently confirmed here -- this client treats each such payload as an opaque
//!   `serde_json::Value` and forwards it, rather than guessing at a typed Rust shape for it.
//! - On completion the executor emits one `TaskArtifactUpdateEvent` (artifact name
//!   `"answer"`, text content) followed by a final `TaskStatusUpdateEvent` with
//!   `state = TASK_STATE_COMPLETED` (success), or `TASK_STATE_FAILED` /
//!   `TASK_STATE_CANCELED` on failure/interruption, each carrying a text message.
//! - Cancellation: A2A's own task-cancel path reaches `HoloExecutor.cancel(context, ...)`,
//!   which resolves the `Session` for that `contextId` and issues a best-effort cancel
//!   against the backend agent-API session, then emits a final
//!   `TaskStatusUpdateEvent(state=TASK_STATE_CANCELED)`. This is "the A2A cancel equivalent"
//!   referenced in the task description.
//!
//! **Spec-inferred, not independently re-derived from `a2a-sdk`'s own source** (the
//! `a2a-sdk` PyPI package itself was not fetched -- only the fact that `holo serve` is a
//! stock `a2a-sdk` server using its stock route helpers):
//!
//! - The exact JSON-RPC 2.0 method names (`message/send`, `message/stream`, `tasks/get`,
//!   `tasks/cancel`), the `Message`/`Task`/`TaskStatusUpdateEvent`/`TaskArtifactUpdateEvent`
//!   JSON field names, and the fact that a streaming call
//!   (`message/stream`) returns `text/event-stream` Server-Sent Events where each `data:`
//!   line is one JSON-RPC 2.0 **response** object (`{"jsonrpc":"2.0","id":...,"result":
//!   <Task | TaskStatusUpdateEvent | TaskArtifactUpdateEvent>}`) are the public, versioned
//!   A2A Protocol specification (<https://a2a-protocol.org>, JSON-RPC transport, protocol
//!   version 0.3.0 -- matching the exact `protocol_version` `holo serve`'s agent card
//!   declares). This is the standard this daemon is coded against; it is not an invention,
//!   but it is *not* re-derived from `holo-desktop-cli`'s own source since the RPC dispatch
//!   itself lives inside the `a2a-sdk` dependency, not in this repo.
//! - If a future `holo-desktop-cli` release changes `a2a-sdk` major version or opts out of
//!   `enable_v0_3_compat`, the wire shape below could drift; the `/health`, agent-card, and
//!   `EVENT_MEDIA_TYPE` details above are the ones this module can and does defend with a
//!   real source citation, so those are checked at runtime (see `A2aClient::new` +
//!   `probe_agent_card`) before the daemon trusts a `holo serve` instance.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use uuid::Uuid;

/// JSON-RPC path `holo serve` mounts its A2A surface on (`create_jsonrpc_routes(handler,
/// "/a2a", ...)` in `serve.py`).
const A2A_RPC_PATH: &str = "/a2a";

/// Media type `holo serve` uses for the data-message payload wrapping each backend
/// `TrajectoryEvent` (`EVENT_MEDIA_TYPE` constant in `serve.py`). Used here only to decide
/// which streamed messages are "raw agent event" vs. a plain status/answer text message.
const HOLO_EVENT_MEDIA_TYPE: &str = "application/vnd.holo-desktop.event+json";

/// One incremental update from a streamed A2A task, translated into the shape the
/// control-channel bridge cares about. Deliberately coarser than the full A2A event union --
/// see module doc for exactly which parts of the wire shape are confirmed vs. spec-inferred.
#[derive(Debug, Clone)]
pub enum TaskUpdate {
    /// Task is running; carries the raw backend `TrajectoryEvent` JSON if this update wrapped
    /// one (`HOLO_EVENT_MEDIA_TYPE`), or a plain human-readable status text otherwise.
    Working {
        raw_event: Option<Value>,
        text: Option<String>,
    },
    /// Final answer text artifact (`TaskArtifactUpdateEvent`, artifact name `"answer"`).
    Answer { text: String },
    /// Terminal state reached.
    Terminal {
        state: TerminalState,
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalState {
    Completed,
    Failed,
    Canceled,
}

/// `Clone` is cheap: `reqwest::Client` is internally `Arc`-based (connection pool shared across
/// clones), and `base_url`/`auth_token` are small owned `String`s. Used by
/// `HoloControlBridge`'s call sites to clone the current client out from behind its `RwLock`
/// before an `.await` (the guard itself can't cross one), and by `HoloBridge::restart_process`
/// to swap in a freshly-built client after respawning `holo serve`.
#[derive(Clone)]
pub struct A2aClient {
    http: reqwest::Client,
    base_url: String,
    auth_token: String,
}

impl A2aClient {
    pub fn new(base_url: String, auth_token: String) -> Self {
        let http = reqwest::Client::builder()
            // No overall timeout: task streams are long-lived by design (a Holo turn can run
            // for the runtime's own max_time_s, up to DEFAULT_MAX_TIME_S=1800s server-side).
            // Per-request connect timeout still applies via the client default.
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client construction with only timeouts set cannot fail");
        Self {
            http,
            base_url,
            auth_token,
        }
    }

    fn rpc_url(&self) -> String {
        format!("{}{A2A_RPC_PATH}", self.base_url)
    }

    /// Confirms the server at `base_url` is actually an A2A server speaking the protocol
    /// version/binding this client is coded against, by fetching its agent card. Fails loudly
    /// rather than silently sending JSON-RPC requests a mismatched server can't parse.
    pub async fn probe_agent_card(&self) -> Result<AgentCardSummary> {
        // Standard A2A well-known discovery path per the public spec
        // (https://a2a-protocol.org): GET /.well-known/agent-card.json. `holo serve` mounts
        // this via `create_agent_card_routes(card)` (confirmed call site in serve.py; the
        // exact path served by that helper is the a2a-sdk's own stock route, not re-derived
        // here beyond the spec's documented default).
        let url = format!("{}/.well-known/agent-card.json", self.base_url);
        // The public A2A spec treats the well-known agent card as unauthenticated discovery,
        // but holo serve's BearerAuthMiddleware guards every route except /health (witnessed
        // live: this GET returns 401 {"error":"unauthorized"} without the token). This daemon
        // always knows the token (it generates it and exports HOLO_AUTH_TOKEN before spawn --
        // see process.rs), so send it unconditionally; a future holo serve that makes the card
        // public simply ignores the header. Without this, HoloBridge::start fails after health,
        // holo serve is SIGTERMed, and the control channel is never mounted -- so a phone
        // connection fails at ALPN negotiation and sees nothing, not even an ack.
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;
        if !resp.status().is_success() {
            bail!("agent card fetch returned {}: GET {url}", resp.status());
        }
        let card: Value = resp
            .json()
            .await
            .with_context(|| format!("agent card at {url} was not valid JSON"))?;

        let streaming = card
            .pointer("/capabilities/streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let protocol_version = card
            .pointer("/supportedInterfaces/0/protocolVersion")
            .or_else(|| card.pointer("/supported_interfaces/0/protocol_version"))
            .and_then(Value::as_str)
            .map(str::to_owned);

        if !streaming {
            bail!(
                "holo serve at {} does not advertise streaming capability in its agent card; \
                 this client requires message/stream support",
                self.base_url
            );
        }

        Ok(AgentCardSummary {
            streaming,
            protocol_version,
            raw: card,
        })
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.auth_token)
    }

    /// Submit a prompt as a new message in `context_id` (pass `None` to start a fresh
    /// context/session) and stream back every update until the task reaches a terminal
    /// state. Calls `on_update` for each one, in order.
    ///
    /// Returns the server-assigned `contextId` actually used, so the caller can persist it
    /// and pass it back in on the *next* prompt to continue the same `hai-agent-runtime`
    /// session rather than starting a new one each time (mirrors how `holo-desktop-cli`'s own
    /// `HoloExecutor._sessions` keys sessions by `contextId`).
    ///
    /// `on_ids` fires as soon as the stream reveals the resolved `contextId` and/or the A2A
    /// `Task.id` (and again if the other resolves later) -- long before the turn's terminal
    /// state. This is what makes a scoped mid-turn `tasks/cancel` (stop/pause/redirect while
    /// the turn is still streaming) possible at all: the return value above only becomes
    /// available after the stream ends, which is exactly too late for anything that wants to
    /// interrupt it. Capturing the real `Task.id` specifically matters because `tasks/cancel`
    /// is keyed by task id per the A2A spec, and this holo serve build genuinely rejects a
    /// context-id stand-in with JSON-RPC `-32603` "Task not found" (live-witnessed 2026-07-21,
    /// falsifying this module's earlier assumption that `HoloExecutor.cancel` resolves by
    /// context -- see [`Self::cancel`]).
    pub async fn send_and_stream<C, F>(
        &self,
        text: &str,
        context_id: Option<&str>,
        mut on_ids: C,
        mut on_update: F,
    ) -> Result<String>
    where
        C: FnMut(ResolvedTurnIds<'_>),
        F: FnMut(TaskUpdate),
    {
        let request_id = Uuid::new_v4().to_string();
        let message_id = Uuid::new_v4().to_string();

        let mut message = json!({
            "role": "user",
            "parts": [{"kind": "text", "text": text}],
            "messageId": message_id,
            "kind": "message",
        });
        if let Some(ctx) = context_id {
            message["contextId"] = json!(ctx);
        }

        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            // Standard A2A streaming RPC method name per the public spec. See module doc:
            // this specific string is spec-inferred, not re-derived from a2a-sdk source.
            "method": "message/stream",
            "params": { "message": message },
        });

        let resp = self
            .http
            .post(self.rpc_url())
            .header("Authorization", self.auth_header())
            .header("Accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {} (message/stream) failed", self.rpc_url()))?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!(
                "holo serve rejected our bearer token (401) -- was HOLO_AUTH_TOKEN changed \
                 after this client was constructed?"
            );
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            bail!("holo serve message/stream returned {status}: {body_text}");
        }

        let mut resolved_context_id = context_id.map(str::to_owned);
        let mut resolved_task_id: Option<String> = None;
        let mut byte_stream = resp.bytes_stream().eventsource();
        let mut terminal_reached = false;

        while let Some(event) = byte_stream.next().await {
            let event = event.context("SSE stream from holo serve errored")?;
            if event.data.trim().is_empty() {
                continue;
            }
            let frame: Value = serde_json::from_str(&event.data).with_context(|| {
                format!("non-JSON SSE data frame from holo serve: {}", event.data)
            })?;

            if let Some(err) = frame.get("error") {
                bail!("holo serve returned a JSON-RPC error: {err}");
            }
            let result = frame
                .get("result")
                .ok_or_else(|| anyhow!("SSE frame had neither `result` nor `error`: {frame}"))?;

            let mut ids_advanced = false;
            if resolved_context_id.is_none() {
                resolved_context_id = result
                    .get("contextId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                ids_advanced |= resolved_context_id.is_some();
            }
            if resolved_task_id.is_none() {
                // The bare initial `Task` object carries its id as `id`; every
                // subsequent status/artifact update event carries it as `taskId`.
                let task_id = match result.get("kind").and_then(Value::as_str) {
                    Some("task") => result.get("id").and_then(Value::as_str),
                    _ => result.get("taskId").and_then(Value::as_str),
                };
                if let Some(id) = task_id {
                    resolved_task_id = Some(id.to_owned());
                    ids_advanced = true;
                }
            }
            if ids_advanced {
                on_ids(ResolvedTurnIds {
                    context_id: resolved_context_id.as_deref(),
                    task_id: resolved_task_id.as_deref(),
                });
            }

            if let Some(update) = parse_task_event(result)? {
                let is_terminal = matches!(update, TaskUpdate::Terminal { .. });
                on_update(update);
                if is_terminal {
                    terminal_reached = true;
                    break;
                }
            }
        }

        if !terminal_reached {
            bail!("holo serve closed the message/stream connection before a terminal task state was observed");
        }

        resolved_context_id
            .ok_or_else(|| anyhow!("stream never reported a contextId for this task"))
    }

    /// Cancel the in-flight task for `context_id` -- the A2A `tasks/cancel` equivalent,
    /// reaching `HoloExecutor.cancel()` server-side (best-effort backend session cancel, see
    /// module doc).
    pub async fn cancel(&self, context_id: &str, task_id: Option<&str>) -> Result<()> {
        let request_id = Uuid::new_v4().to_string();
        // Per the public A2A spec, tasks/cancel takes a TaskIdParams keyed by TASK id --
        // callers should always pass the real `task_id` captured via `send_and_stream`'s
        // `on_ids`. The context-id fallback below is kept only for callers that genuinely
        // never saw a task id, and is now KNOWN not to work against the current holo serve:
        // live-witnessed 2026-07-21, a context-id cancel returns JSON-RPC -32603
        // "Task not found" (this module's earlier claim that HoloExecutor.cancel resolves by
        // context was wrong for this build). Callers therefore treat a cancel error as
        // "fall back to the global `holo stop`", which always works.
        let id_for_cancel = task_id.unwrap_or(context_id);
        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tasks/cancel",
            "params": { "id": id_for_cancel },
        });

        let resp = self
            .http
            .post(self.rpc_url())
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {} (tasks/cancel) failed", self.rpc_url()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            bail!("holo serve tasks/cancel returned {status}: {body_text}");
        }
        let frame: Value = resp.json().await.context("tasks/cancel response was not valid JSON")?;
        if let Some(err) = frame.get("error") {
            bail!("holo serve tasks/cancel returned a JSON-RPC error: {err}");
        }
        Ok(())
    }
}

/// The turn-identifying ids [`A2aClient::send_and_stream`] has resolved so far, delivered via
/// its `on_ids` callback the moment either advances: `context_id` (the session-continuity key,
/// for continuing the conversation on a later turn) and `task_id` (the A2A `Task.id`, the key
/// `tasks/cancel` actually requires).
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTurnIds<'a> {
    pub context_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
}

pub struct AgentCardSummary {
    pub streaming: bool,
    pub protocol_version: Option<String>,
    /// The full agent card, kept for callers that want to log/inspect fields beyond the two
    /// summarized above (name, skills, capabilities). Not read internally today -- this
    /// client only acts on `streaming`/`protocol_version` -- but dropping it would throw away
    /// data already fetched for a field a future caller (e.g. a diagnostics/`whoami` surface)
    /// is very likely to want.
    #[allow(dead_code)]
    pub raw: Value,
}

/// Parse one JSON-RPC `result` payload from the `message/stream` SSE body into a
/// [`TaskUpdate`], or `None` for a shape this client intentionally ignores (e.g. the initial
/// bare `Task` object some A2A servers emit before the first status update).
fn parse_task_event(result: &Value) -> Result<Option<TaskUpdate>> {
    let kind = result.get("kind").and_then(Value::as_str);

    match kind {
        Some("status-update") => {
            let state = result
                .pointer("/status/state")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("status-update missing status.state: {result}"))?;

            let message_text = extract_text_message(result.pointer("/status/message"));
            let raw_event = extract_data_message(result.pointer("/status/message"));

            match state {
                "TASK_STATE_WORKING" | "working" => Ok(Some(TaskUpdate::Working {
                    raw_event,
                    text: message_text,
                })),
                "TASK_STATE_COMPLETED" | "completed" => Ok(Some(TaskUpdate::Terminal {
                    state: TerminalState::Completed,
                    message: message_text,
                })),
                "TASK_STATE_FAILED" | "failed" => Ok(Some(TaskUpdate::Terminal {
                    state: TerminalState::Failed,
                    message: message_text,
                })),
                "TASK_STATE_CANCELED" | "canceled" | "cancelled" => Ok(Some(TaskUpdate::Terminal {
                    state: TerminalState::Canceled,
                    message: message_text,
                })),
                // submitted / auth-required / rejected / unknown: not a case
                // holo-desktop-cli's HoloExecutor emits (it only ever moves
                // WORKING -> {COMPLETED,FAILED,CANCELED}), and not one this bridge has a
                // defined behavior for; surface it as a working-with-text update rather than
                // silently dropping it or guessing at a terminal mapping.
                other => Ok(Some(TaskUpdate::Working {
                    raw_event: None,
                    text: Some(format!("unrecognized task state {other:?}")),
                })),
            }
        }
        Some("artifact-update") => {
            let name = result.pointer("/artifact/name").and_then(Value::as_str);
            if name != Some("answer") {
                return Ok(None);
            }
            let text = result
                .pointer("/artifact/parts/0/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Ok(Some(TaskUpdate::Answer { text }))
        }
        // A bare Task object (kind == "task") on first response, or any other envelope kind:
        // nothing actionable for the bridge yet.
        _ => Ok(None),
    }
}

fn extract_text_message(message: Option<&Value>) -> Option<String> {
    let message = message?;
    let parts = message.get("parts")?.as_array()?;
    let mut out = String::new();
    for part in parts {
        if part.get("kind").and_then(Value::as_str) == Some("text") {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn extract_data_message(message: Option<&Value>) -> Option<Value> {
    let message = message?;
    let parts = message.get("parts")?.as_array()?;
    for part in parts {
        let is_holo_event = part.get("kind").and_then(Value::as_str) == Some("data")
            && part
                .get("metadata")
                .and_then(|m| m.get("mediaType").or_else(|| m.get("media_type")))
                .and_then(Value::as_str)
                == Some(HOLO_EVENT_MEDIA_TYPE);
        if is_holo_event {
            return part.get("data").cloned();
        }
    }
    None
}
