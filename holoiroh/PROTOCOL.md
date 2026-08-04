# HoloIroh control-channel protocol

This document defines the control channel JavaScript Object Notation (JSON)
message schema. The app (`ios/`) and daemon (`mac-daemon/`) exchange these
messages. The control channel is a second bidirectional logical stream.
It runs alongside the `iroh-live` media broadcast, on the same `iroh`
`Endpoint`. See `README.md`'s "Control channel" section and "Why iroh /
iroh-live specifically" section for the architecture rationale.

This document is the source of truth for the wire schema. The Rust types
that implement the schema via `serde` (`ClientMessage`, `ServerMessage`,
`TaskEnvelope<T>`) live in the `holoiroh-wire` crate
(`holoiroh-wire/src/lib.rs`). This lets both `mac-daemon` and `ios-bridge`
share one definition instead of duplicating it. `ios-bridge` is the iOS foreign function interface (FFI) crate. It must cross-compile to `aarch64-apple-ios`. It cannot depend
on `mac-daemon`'s macOS-only `holo_bridge`/`audit_log` modules.
`mac-daemon/src/control_channel.rs` re-exports them at the same
`control_channel::{ClientMessage, ServerMessage, TaskEnvelope, ...}`
paths. It also owns the connection-handling logic that uses this schema. This logic includes:

- The `iroh` `ProtocolHandler` implementation.
- The PIN and allowlist authentication gate.
- The per-connection sequence state.

Any schema change requires a matching change in
`holoiroh-wire/src/lib.rs`. It eventually also requires a Swift client change.

## Project Aro product requirements document (PRD) context: six logical streams, one implemented

The Project Aro PRD names **six** logical streams for this system: `control`,
`pairing`, `media`, `manual_input`, `snapshot_fallback`, and `telemetry`.
**Only `control` is implemented in this codebase today.** This document
covers that one stream exclusively. The other five are PRD-tracked,
not-yet-built work. Do not read anything below as a description of them.
`pairing`'s narrower incremental precursor (PIN + device allowlist, no
envelope wrapping of its own -- see "Envelope" below) is real. It is
documented in [`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md). This
precursor is a distinct, already-existing mechanism layered *underneath*
the control stream's auth gate. It is not the PRD's `pairing` stream
itself.

## Transport

- **Application-Layer Protocol Negotiation (ALPN):** `holoiroh/control/1`.
  This is a dedicated ALPN string. The same `iroh::Endpoint` and
  `iroh::protocol::Router` register the control and media protocols. They also
  register gossip ALPNs when gossip is enabled. The media protocol is
  `iroh-live`'s Media over QUIC (MoQ) protocol, `iroh-moq`. See
  `iroh_live::Live::register_protocols`. This project's
  `ControlChannel::register_protocols` mirrors that function. The control
  channel and media stream use the same endpoint and peer identity
  (`EndpointId`). Each ALPN uses an independent `iroh::endpoint::Connection`.
  Each connection has an independent lifecycle and reconnect process. Both
  connections use the same path-selection machinery. This machinery provides
  network address translation (NAT) traversal and relay fallback.
- **Stream:** one bidirectional QUIC stream per control-channel
  connection. The dial side opens it via `Connection::open_bi()`. The
  accept side accepts it via `Connection::accept_bi()`, inside the
  `ProtocolHandler::accept` callback.
- **Framing:** newline-delimited JSON (NDJSON). Each message is one JSON
  object terminated by `\n`. Both Rust network readers enforce a 96 MiB authenticated-frame limit.
  They enforce this limit while scanning for the newline. They reject invalid
  Unicode Transformation Format 8-bit (UTF-8) before JSON or signature processing. Thus, a peer cannot force
  an unbounded line allocation before verification.
- **Direction:** the iOS app is the dial side and the Mac daemon is the
  accept side:
  - The iOS app sends `ClientMessage` values.
  - The Mac daemon sends `ServerMessage` values.

  Both directions share the same bidirectional stream (`SendStream` +
  `RecvStream` pair from the one `accept_bi()`/`open_bi()` call). The
  daemon reads `ClientMessage` off `RecvStream` while it writes
  `ServerMessage` onto `SendStream`. The iOS side does the mirror image:
  it reads `ServerMessage` off `RecvStream` while it writes
  `ClientMessage` onto `SendStream`.

## Envelope

The sender wraps every control channel message in a `TaskEnvelope`. The
pre-session personal identification number (PIN) handshake is the only exception. See "The one exception:
the PIN handshake is unwrapped" below. This matches the Project Aro PRD's
authoritative task-envelope shape:

```json
{
  "protocol_version": 1,
  "message_id": "d5c2a236-6c32-4cd1-baa7-27a24930b423",
  "session_id": "5e0e6e0a-2222-4a3f-9c1a-7b8b6b6b6b6b",
  "task_id": "8f2b6b6b-3333-4a3f-9c1a-7b8b6b6b6b6b",
  "message_type": "prompt",
  "sent_at": 1784349135135,
  "expires_at": 1784349165135,
  "sequence_number": 0,
  "payload": { "type": "prompt", "text": "open safari and check my calendar" },
  "signature": "ed25519:<128 lowercase hexadecimal characters>"
}
```

| Field              | Type                | Required | Meaning |
|--------------------|---------------------|----------|---------|
| `protocol_version` | `u32`                | yes | This is the envelope schema's version. It is currently always `1` (`control_channel::PROTOCOL_VERSION`). Both network boundaries reject any other value. |
| `message_id`       | `string` (uuid v4)   | yes | Each message has a unique `message_id`, minted fresh by whichever side sends it. The daemon uses it for duplicate-message rejection (see "Rejection rules" below). |
| `session_id`       | `string` (uuid v4)   | yes | The daemon mints `session_id` once per accepted `iroh` connection (see "Session lifecycle" below). It stays stable for that connection's lifetime. Every envelope on a given connection carries the same `session_id`, in either direction. |
| `task_id`          | `string` \| `null`   | no  | `task_id` correlates an envelope with a specific bridge turn. A turn is a `prompt`/`voice_transcript`/`stop` and the `ack`/`status`/`task_progress`/`error` replies it produces. It is `null` or omitted for envelopes with no turn to correlate to, such as the initial greeting or a reconnect status update. See "task_id threading" below. |
| `message_type`     | `string`             | yes | This must exactly equal the typed payload's `type` discriminant. Both network boundaries reject a mismatch after signature verification and typed payload parsing. |
| `sent_at`          | `u64` (unix ms)      | yes | The time when the sender constructed this envelope. |
| `expires_at`       | `u64` (unix ms)      | yes | The receiver rejects this envelope if it arrives after this instant. `TaskEnvelope::new`/`wrap` default `expires_at` to `sent_at + 30_000` (30s) when they construct the envelope; see "Rejection rules". |
| `sequence_number`  | `u64`                | yes | `sequence_number` must strictly increase per `session_id`, per direction (see "Rejection rules" and "Two independent sequences, per direction" below). It starts at `0` for the first envelope either side sends on a fresh connection. |
| `payload`          | `ClientMessage` \| `ServerMessage` | yes | The actual message content. It has exactly the `{type, text?}` (or `{type, pin}`) shape documented below in "`ClientMessage`"/"`ServerMessage`". Envelope-wrapping does not change this shape. |
| `signature`        | `string`             | yes after auth | Strict `ed25519:` plus 128 lowercase hexadecimal characters. The signer is the authenticated iroh transport peer. Missing, malformed, uppercase, wrong-key, or invalid signatures are rejected before typed payload parsing or mutable envelope state. |

The serializer omits `task_id` and `signature` when they are absent. This is
useful only while an in-process constructor is assembling an envelope. The
single daemon writer and the Rust iOS bridge attach `signature` immediately
before serialization and network write. A post-auth network envelope with an
omitted signature is invalid. `task_id` remains legitimately optional.

### Envelope signatures

Every post-auth envelope uses Ed25519 with the same identity that authenticated
its iroh connection. The daemon signer is `Endpoint::secret_key()` from the
persisted Live endpoint. The client signer is the iOS bridge endpoint key. The
verifier is always `Connection::remote_id()`; no signing key is transmitted in
JSON or exposed through a signing FFI.

`TaskEnvelope::signing_payload` is the only canonical encoding. It separates
the `holoiroh/control/1`, `task-envelope`, `ed25519`, and `signature-v1` domains.
It binds the `ClientToDaemon` or `DaemonToClient` direction. It binds the signer
and recipient 32-byte public keys. It length-prefixes every envelope field. Integer fields
use fixed-width big-endian bytes. Payload objects use recursively sorted UTF-8
keys and explicit JSON type tags. The `signature` field itself is excluded.

Verification order is fail-closed:

1. Parse only the bounded envelope shell.
2. Require the strict signature codec, rebuild canonical bytes for the expected
   direction and authenticated transport identities, and verify Ed25519.
3. Parse the typed payload and require `message_type == payload.type`.
4. Require protocol version and session, then validate expiry, replay ID, and
   sequence.
5. Only after all checks may the receiver mutate correlation/task/application
   state or dispatch the payload.

A rejection before step 4 consumes neither `message_id` nor sequence state.
The daemon returns a bounded signed error and continues reading. The iOS bridge stores a bounded fatal reader error. It closes the invalid
stream. The polling application binary interface (ABI) reports failure and
does not queue the invalid event.

### The one exception: the PIN handshake is unwrapped

`session_id` does not exist yet at two points. It does not exist when an
unrecognized device sends a `pin` message. It also does not exist when the
daemon sends back an `auth_rejected` reply, if the PIN is wrong or
missing. The daemon only mints a `session_id` **after** the auth gate
passes (see "Session lifecycle" below).

Wrapping a message in a `session_id`-bearing envelope before one exists
has two possible costs:

- A placeholder or empty `session_id`, which would be misleading since it
  is not really *this* connection's session.
- Delaying session-minting further, which would break the auth gate's
  existing contract of reading exactly one line before anything else
  happens.

Given these costs, **the client sends the `pin` `ClientMessage` as bare,
unwrapped JSON**. **The daemon sends the `auth_rejected` `ServerMessage`
the same way**. Both match exactly the pre-envelope wire shape
(`{"type":"pin","pin":"..."}`, `{"type":"auth_rejected","text":"..."}`).
The sender envelope-wraps every message from the "control channel ready"
greeting onward. This greeting is the first message the daemon sends
*after* it mints a `session_id`. This is a real architectural boundary in
`control_channel.rs`, not an oversight. Search for
`ControlChannel::authenticate` to see the exact point where the switch
happens.

Already-allowlisted devices, and daemons run with `--no-pin-auth`, never
send or receive `pin`/`auth_rejected` at all. For them, the very first
message on the stream is the envelope-wrapped greeting.

### Session lifecycle

The daemon mints a `session_id` (`uuid::Uuid::new_v4()`, universally unique identifier version 4) once per accepted
`iroh` connection. It mints the `session_id` immediately after the
PIN/allowlist auth gate passes. The daemon skips that gate when the
device is already allowlisted, or when `--no-pin-auth` is set.

`session_id` has no relationship to the persisted device allowlist or to
`iroh`'s own connection/node identity. A reconnecting device gets a fresh
`session_id` on its next connection. This is true even for the same
allowlisted device, and even immediately after a clean disconnect.

The daemon does not persist anything about `session_id` across
connections or daemon restarts. This includes the sequence-number and
seen-`message_id` state scoped to it (see "Rejection rules").

### Two independent sequences, per direction

`sequence_number` is **not** one shared counter for the whole connection.
Each direction has its own counter. Each counter starts at `0`
independently:

- The daemon numbers its own outgoing `ServerMessage` envelopes `0, 1, 2,
  ...` in the order it sends them. The greeting is always `0`.
- The daemon validates the client's incoming `ClientMessage` envelopes
  against their own, separately-tracked last-seen `sequence_number`. The
  daemon does not care what number the client used relative to what the
  daemon itself sent.

The protocol scopes the envelope per `session_id` and per direction. The
envelope does not use one global order for both connection directions.

### task_id threading

- If present, an inbound `ClientMessage` envelope's `task_id` becomes the
  daemon's internal `request_id`. The daemon forwards it to `holo_bridge`. To correlate a request with its replies, the client should set `task_id`.
  Requests include `prompt`, `voice_transcript`, and `stop`. Replies include
  `ack`, `task_progress`, and `error`. The
  daemon echoes that `task_id` back on every reply envelope for that turn.
- If an inbound envelope omits `task_id`, the daemon synthesizes a fresh
  `uuid` for it. This matches the daemon's pre-envelope behavior, where it
  always synthesized a `request_id` regardless of what the client sent.
  That synthesized id becomes the `task_id` on the reply envelopes for
  that turn. A client can still observe which replies belong together,
  even without providing its own id up front.
- The `pin` message and the initial "control channel ready"
  greeting/reconnect-status messages have no `task_id`. There is no
  bridge turn to correlate to yet.

## Rejection rules

Both directions apply the verification order in "Envelope signatures". The
signature gate precedes typed payload parsing and all mutable replay/session/
sequence or task state. After a valid signature and typed payload/message-type
match, the receiver applies these state rules:

1. **Session.** `session_id` must equal the nonempty session established by the
   verified ready greeting.
2. **Expiry.** Current time must not be strictly greater than `expires_at`.
3. **Duplicate `message_id`.** A previously accepted ID in this direction and
   session is rejected.
4. **Sequence monotonicity.** The first client envelope is sequence `0`; later
   values must strictly increase. The daemon's greeting is server sequence `0`.

All checks complete before replay ID or sequence is recorded. These failures cannot consume the final valid sequence `0`:

- Missing or invalid encoding.
- Wrong key or signature tampering.
- Payload or metadata tampering.
- Wrong session or expiry.
- Duplicate ID or sequence rejection.

The daemon never forwards a rejected envelope to `holo_bridge`. It emits a
bounded signed `error` with no rejected task correlation, then keeps reading.
The bridge never queues a rejected daemon envelope; polling reports its bounded
failure and the connection must reconnect.

## `ClientMessage` (iOS → Mac daemon)

The `payload` field of every inbound `TaskEnvelope` (or, for `pin` only, the
entire bare message body):

```json
{ "type": "prompt", "text": "open safari and check my calendar" }
{ "type": "voice_transcript", "text": "what's on my screen right now" }
{ "type": "stop" }
{ "type": "pause" }
{ "type": "resume" }
{ "type": "redirect", "text": "actually, draft it as an email instead" }
{ "type": "pin", "pin": "123456" }
{ "type": "input_response", "request_id": "d290f1ee-6c54-4b01-90e6-d701748f0851", "selected_option": "Work calendar" }
{ "type": "clarify_request", "prompt": "send a message to the team" }
{ "type": "remote_control", "event": { "action": "take_control" } }
{ "type": "remote_control", "event": { "action": "move", "x": 0.5, "y": 0.4 } }
{ "type": "remote_control", "event": { "action": "click", "x": 0.5, "y": 0.4, "button": "left", "count": 1 } }
{ "type": "remote_control", "event": { "action": "scroll", "x": 0.5, "y": 0.4, "dx": 0.0, "dy": -3.0 } }
{ "type": "remote_control", "event": { "action": "text", "text": "hello team" } }
{ "type": "remote_control", "event": { "action": "release_control" } }
```

| Field             | Type                                                                        | Required | Meaning |
|-------------------|------------------------------------------------------------------------------|----------|---------|
| `type`            | `"prompt"` \| `"voice_transcript"` \| `"stop"` \| `"pause"` \| `"resume"` \| `"redirect"` \| `"pin"` \| `"input_response"` \| `"remote_control"` \| `"clarify_request"` \| `"process_document"` \| `"analyze_image"` \| `"transcribe_audio"` \| `"request_speech"` \| `"plan_task"` | yes | Discriminant. |
| `event`           | `RemoteControlEvent`                                                          | only for `remote_control` | This field is the touch-derived action. It is itself tagged by `action` (see below). |
| `text`            | `string`                                                                    | only for `prompt` / `voice_transcript` / `redirect` / `request_speech` | Instruction or speech text. The default `voice_transcript` path carries client-side text, not raw audio. |
| `prompt`          | `string`                                                                    | only for `clarify_request` / `analyze_image` | The instruction to clarify or the question to answer about an image. |
| `pin`             | `string`                                                                    | only for `pin` | The PIN value. The Mac terminal shows this PIN to the user out-of-band, alongside the ticket. The user types or scans it into the client. |
| `request_id`      | `string`                                                                    | only for `input_response` | This field echoes the `request_id` of the `input_request` it answers. |
| `selected_option` | `string`                                                                    | only for `input_response` | The user's chosen option. It must be one of the original `input_request.response_options`. |

- `prompt`: a typed text instruction. The daemon hands it to the
  `holo-desktop-cli` bridge as-is.
- `voice_transcript`: functionally identical to `prompt` on the wire,
  using the same `text` field. The daemon tags it separately to identify the
  input modality. The user interface (UI) can use this tag for logging or user
  experience (UX). For example, the status panel can show a mic icon without
  re-deriving the modality from context.
- `stop`: the remote **kill-switch**. It cancels or interrupts agent work
  on the Mac. It is one variant with two forms:
  - **Global form** (`{"type":"stop"}`, `context_id` absent): stop
    *everything*. On the daemon this maps, via
    `control_channel::to_control_message`, to `ControlMessage::Stop` with
    `context_id: None`. `HoloControlBridge::handle_stop` uses this order:
    1. It scoped-cancels the current turn through agent-to-agent (A2A)
       `tasks/cancel`. It uses the daemon's resolved `contextId` and `Task.id`.
       The client never has these values.
    2. It drains any queued prompts. Each queued prompt gets a terminal
       `status`/`Done{Canceled}` with `"canceled: stop requested while
       queued"`.
    3. It discards any paused turn outright.
    4. It engages the CLI-level global kill switch by shelling out to
       `holo stop` (see `mac-daemon/src/holo_bridge/stop.rs`).

    This produces the same pause-then-cancel effect as the double-Esc /
    `holo stop` kill switch built into `holo-desktop-cli` itself. If the
    same turn is still running ~3s later, the daemon escalates with
    `holo stop --force`.
  - **Scoped form** (`{"type":"stop","context_id":"<a2a contextId>"}`):
    cancel ONE specific turn. The daemon issues A2A `tasks/cancel` for
    exactly that context. It uses the real `Task.id` when the target is the current turn.
    A `Task.id` can come only from this location. A context-id-only cancel returns JSON-RPC `-32603` against
    the current `holo serve`. **None** of the all-or-nothing machinery
    runs for a scoped stop: no queue drain, no global `holo stop`, no
    force escalation. The daemon discards a paused turn stashed under the
    *same* `context_id`, since it would otherwise resurrect on the next
    `resume`. Stashes of other contexts survive. A scoped stop naming a
    context with nothing running resolves to a polite `status` note
    (`stop: cancel requested for context ... (no turn with that context
    is running here)`); it never triggers a global stop of an unrelated
    turn. On a failed cancel, the stop surfaces an `error` event (`A2A
    cancel failed for context ...`). It does **not** report a false
    `Done`.

  Serde compatibility: the scoped form is additive per this protocol's
  extension policy. The `context_id` field is `Option<String>` with
  `skip_serializing_if = "None"`. Because of this, the global form
  serializes byte-identically to the pre-field unit variant
  (`{"type":"stop"}`). Old `{"type":"stop"}` payloads deserialize
  unchanged.
- `pause`: parks the in-flight turn so `resume` can continue it. The Holo
  backend exposes **no pause remote procedure call (RPC)** over A2A. Its kill
  switch uses pause-then-cancel. See the source notes in
  `mac-daemon/src/holo_bridge/stop.rs`. Because of this, the daemon implements pause as the only honest
  primitive available: it **cancels** the running turn. It uses scoped
  A2A `tasks/cancel` once the turn's `contextId` resolves, or else the
  graceful global `holo stop`. While it does this, it stashes the turn's
  original instruction text and `contextId`. The canceled turn still produces its normal `task_done` (`canceled`). A
  client that shows a "paused" state must accept that terminal event. Pausing with
  nothing running, or pausing twice, is a polite `status` reply, never an
  error.
- `resume`: re-dispatches the parked turn on the **same** `contextId`. The
  backend session history continues the task from its interruption point. The resumed turn runs under the `resume` envelope's
  own `task_id`. Resuming with nothing parked is a polite `status` reply.
- `redirect`: replaces whatever is running or queued with a new
  instruction. The daemon does the following, in order:
  1. Cancels the in-flight turn.
  2. Drains the queue. Each queued prompt gets its own
     `task_done`/`Done{Canceled}`.
  3. Discards any parked (paused) turn.
  4. Runs `text`, reusing the canceled turn's `contextId` when known, so
     the agent keeps the task history it built.

  The daemon rejects an empty `text` with an `error`.
- `pin`: presents a PIN for first-connection auth (see
  `mac-daemon/PAIRING.md`'s "Auth beyond ticket possession" section). The
  client whose device id is not already in the Mac's allowlist must send
  this as the **first** message. The daemon does not send its normal
  greeting (`status: "control channel ready"`) until this gate passes for
  an unrecognized device. A client should always be prepared to send
  `pin` before anything else on a fresh connection to an unfamiliar Mac.
  The client sends `pin` **unwrapped**, not inside a `TaskEnvelope` (see
  "The one exception" above). Already-allowlisted devices, or a daemon
  run with `--no-pin-auth`, never need to send this message at all. A
  client cannot know in advance whether the daemon will prompt it, so it
  may send `pin` anyway, still unwrapped. In that case the daemon
  acknowledges and ignores it (see `ControlChannel::accept`'s handling of
  a redundant `Pin`).
- `input_response`: the user's answer to a `ServerMessage`'s
  `input_request` (see below). This message carries a **structured
  choice selection only**. `selected_option` must be one of the strings
  the matching `input_request.response_options` offered. This message deliberately has no free-text field. Thus, a client cannot put
  a password, PIN, or multi-factor authentication (MFA) code into it. `request_id` might not match an outstanding `input_request` on this
  connection. The request can be expired, answered, or never sent. In that case, the daemon replies with a normal `error`
  event, and the connection stays open. This is not a transport-level
  failure. It matches this document's general "malformed input"
  philosophy below. See the `input_request` entry for the complete contract. It explains why
  this message does **not** carry credentials, MFA codes, or manual input.
- `clarify_request`: asks the daemon to generate zero to three structured
  questions before a prompt runs. `prompt` is capped before inference. The
  daemon handles this message outside the desktop-task pipeline and replies
  with `clarify_questions`. An empty question list means no clarification is
  needed and the client can send the original `prompt`.
- `remote_control`: the user escalates to **hands-on control**. The user
  drives the Mac directly by touching the iOS live-share view. The
  `event` object is a nested tagged action (`{"action": ...}`):
  - `take_control` / `release_control`: enter or leave control. The
    daemon PAUSES any active agent turn on `take_control`. This is a user
    pause, so cooperative auto-yield does not auto-resume it. The daemon
    RESUMES the turn on `release_control`.
  - `move`/`button`/`click`/`scroll`: pointer actions at a **normalized**
    point (`x`,`y` in `0.0..=1.0` within the captured display). The
    daemon maps them to real display points. `button` carries `button`
    (`"left"`/`"right"`) and `down`. `click` carries `button` and `count`
    (2 = double-click). `scroll` carries `dx`/`dy` wheel deltas.
  - `text`: types a unicode string. `key`: presses or releases a named
    special key (`"return"`, `"delete"`, `"escape"`, arrows, …).

  Injection requires macOS Accessibility (`AXIsProcessTrusted`). Without
  it, the daemon emits a one-time `status` message asking the user to
  grant it, then no-ops the input. This message type is additive per
  this document's extension policy.

### Tinfoil-backed messages: `process_document` / `analyze_image` / `transcribe_audio` / `request_speech` / `plan_task`

This section adds five additive message types for the Tinfoil
confidential-computing integration (see README's "Aro Confidential Cloud
(Tinfoil)" section). The daemon handles all five **off** the desktop-task
pipeline, the same way it handles `clarify_request` (see below). None of
them ever dispatches to `holo-desktop-cli`. The daemon independently
disables each one when no key is configured, replying with its
`*_failed` counterpart (`error: "no TINFOIL_API_KEY configured"`).

```json
{ "type": "process_document", "request_id": "...", "filename": "notes.pdf", "data_base64": "...", "mode": "text" }
{ "type": "analyze_image", "request_id": "...", "image_data_base64": "...", "prompt": "what is in this screenshot?" }
{ "type": "transcribe_audio", "request_id": "...", "audio_data_base64": "...", "format": "wav" }
{ "type": "request_speech", "request_id": "...", "text": "hi", "voice": "serena" }
{ "type": "plan_task", "request_id": "...", "goal": "reply to the last email from Sam and archive it" }
```

- `process_document`: converts an attached document to markdown
  (`tinfoil_documents.rs`, `/v1/convert/file`). `mode` is one of
  `"text"`/`"vision"`/`"images"`/`"raw"`/`"vlm"`. Unrecognized values
  default to `"text"`.
- `analyze_image`: answers `prompt` about an attached image
  (`tinfoil_vision.rs`). The daemon redacts the image on-device before egress.
  `privacy.rs` performs optical character recognition (OCR) and personally
  identifiable information (PII) redaction.
- `transcribe_audio`: transcribes audio (`tinfoil_audio.rs`,
  `voxtral-small-24b`). **This message must carry only audio from the client microphone.** Never send
  system or speaker output. That output can contain other participants' voices. This is an explicit opt-in alternative to the default
  on-device `voice_transcript` path (`VoiceTranscriber.swift`'s on-device
  Speech framework). Sending this message means audio leaves the device.
  `format` is advisory only. Tinfoil infers the real format from
  content.
- `request_speech`: synthesizes `text` as speech (`tinfoil_audio.rs`,
  `qwen3-tts`). It returns WAV bytes.
- `plan_task`: asks Tinfoil's `glm-5.2` (tool-calling) to break `goal`
  into an ordered step list for the user to review. **This message
  plans; it does not execute.** Turning a step into action still
  requires a separate `prompt` or `remote_control` message. This is how
  this message works today.

### Typed screen-observation planner protocol

The schema also defines `typed_prompt` for the typed planner path:

```json
{
  "type": "typed_prompt",
  "prompt": {
    "goal_id": "goal-42",
    "instruction": "Open Settings and show the display controls"
  }
}
```

The signed `TaskEnvelope` gives this `TypedPrompt` its authority. The verified
`goal_id` and `instruction` are the only trusted goal input. Screen pixels,
Accessibility (AX) data, document text, and tool output are untrusted
observation data. They can describe state. They cannot change the goal, grant
approval, or add an action.

The planner accepts one `submit_plan` tool call. It rejects prose, unknown
fields, unknown tools, extra choices, and extra tool calls. The plan contains
one through 64 steps. It ends with exactly one terminal `complete` step.
The only action vocabulary is:

- `observe`.
- `navigate` with `semantic_activate`, `coordinate_activate`, or `scroll`.
- `focus`.
- `draft_text`.
- `commit` with `send_message`, `submit_form`, `publish`, `purchase`,
  `transfer_funds`, or `delete_item`.

Each action binds the goal digest, run, task, action, observation, target, and
proposal digest. The target includes bundle, window, element, expected AX role,
title digest, optional value digest, and resolved, sensitive, and credential
flags. Unknown action types and malformed bindings are unsupported. They execute
nothing.

The planner request has these implemented bounds:

- The goal is at most 16,384 bytes.
- The observation is at most 65,536 bytes.
- The plan has at most 64 steps.
- Tool arguments are at most 524,288 bytes.
- The response is at most 1,048,576 bytes.
- The Tinfoil request timeout is 60 seconds.

A stop, pause, redirect, disconnect, or terminal task event invalidates pending
approvals. Denial, cancellation, expiry, replay, or stale state also stops the
action. The typed path must not continue through Holo after any typed failure.

`typed_plan_ready`, `planner_status`, and `planner_receipt` are the typed reply
variants. `planner_status.status` is `planning`, `ready`, `executing`,
`completed`, `failed`, or `canceled`. A receipt binds the plan and goal digests,
action and proposal identifiers, and terminal status.

These types do not make a live planner loop. The daemon does not currently
handle `typed_prompt` on its live control path. It does not route a plan into a
screen-observation loop or a macOS AX action adapter. Therefore, autonomous
execution of every typed plan action is currently unsupported. This includes
commit actions, sensitive-target mutations, and credential entry. The daemon
must report unsupported behavior. It must not route these actions to Holo.

A future live integration must route every typed action through the daemon-owned
`DaemonActionExecutor`. It must recapture semantic AX state before each action.
The bundle, window, element, role, title digest, and value digest must match.
Coordinate proximity alone is not a semantic precondition.

The model must never receive approval text. The app can send only the signed,
action-bound `approval_response`. The daemon verifies and consumes this typed
response outside planner inference. Approval does not change the trusted goal
and does not add model context.

Hosted planner egress must use only the attested Tinfoil client and its verified
origin. There is no generic hosted-model fallback. A local planner mode must use
a loopback-only endpoint and must not make network egress. The current code has
no live typed planner loop or local typed planner mode.

## `ServerMessage` (Mac daemon → iOS)

The `payload` field of every outbound `TaskEnvelope` (or, for
`auth_rejected` only, the entire bare message body):

```json
{ "type": "ack" }
{ "type": "status", "text": "connected to holo-desktop-cli" }
{ "type": "task_progress", "text": "clicked Safari icon in the Dock" }
{ "type": "task_done", "status": "completed", "text": "drafted the message" }
{ "type": "task_active", "paused": false, "queued": 0 }
{ "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
{ "type": "auth_rejected", "text": "incorrect PIN" }
{ "type": "current_ticket", "ticket": "iroh-live:.../holoiroh" }
{ "type": "clarify_questions", "questions": [{ "question": "Which team?", "options": ["Design", "Engineering"] }] }
{ "type": "secure_input_state", "active": true }
{ "type": "tinfoil_verification", "host": "https://router.inf6.tinfoil.sh", "ground_truth": { "digest": "173ed0...", "tls_public_key": "...", "hpke_public_key": "...", "code_measurement": { "type": "sev-snp", "registers": ["..."] }, "enclave_measurement": { "type": "sev-snp", "registers": ["..."] }, "code_fingerprint": "...", "enclave_fingerprint": "..." } }
{ "type": "input_request", "request_id": "d290f1ee-6c54-4b01-90e6-d701748f0851", "kind": "ambiguous_choice", "context": "Two calendars match 'team standup' -- which one?", "response_options": ["Work calendar", "Personal calendar"], "expires_at": 1800000120000 }
```

| Field              | Type                                                                                          | Required | Meaning |
|--------------------|-------------------------------------------------------------------------------------------------|----------|---------|
| `type`             | `"ack"` \| `"status"` \| `"error"` \| `"task_progress"` \| `"task_done"` \| `"task_active"` \| `"auth_rejected"` \| `"current_ticket"` \| `"clarify_questions"` \| `"input_request"` \| `"secure_input_state"` \| `"tinfoil_verification"` \| Tinfoil reply variants below | yes | Discriminant. |
| `text`             | `string`                                                                                       | optional on `ack`/`status`/`error`/`task_progress`/`task_done`/`auth_rejected` | Human-readable detail. |
| `ticket`           | `string`                                                                                       | only for `current_ticket` | The authenticated daemon's current node ticket. |
| `questions`        | `ClarifyingQuestion[]`                                                                         | only for `clarify_questions` | Zero to three questions, each with concrete `options`. |
| `active`           | `bool`                                                                                         | only for `secure_input_state` | Whether macOS secure input is active. |
| `host`             | `string`                                                                                       | only for `tinfoil_verification` | The verified HTTPS origin used for Tinfoil egress. |
| `ground_truth`     | `TinfoilGroundTruth`                                                                           | only for `tinfoil_verification` | Verified release digest, measurements, TLS/HPKE keys, and code/enclave fingerprints. |
| `status`           | `"completed"` \| `"failed"` \| `"canceled"`                                                     | only for `task_done` | Which terminal state the task reached. |
| `paused`           | `bool`                                                                                          | only for `task_active` (defaults `false`) | Whether the still-live task is parked (Resume/Stop) or running (Pause/Stop). |
| `queued`           | `number`                                                                                        | only for `task_active` (defaults `0`) | How many prompts are queued behind the still-live task. |
| `request_id`       | `string`                                                                                       | only for `input_request` | This field correlates this request with the eventual `ClientMessage.input_response`. |
| `kind`             | `"credential"` \| `"mfa"` \| `"ambiguous_choice"` \| `"missing_info"` \| `"sensitive_access_consent"` | only for `input_request` | Classifies *why* the daemon needs input — see below. |
| `context`          | `string`                                                                                       | only for `input_request` | Human-readable explanation of what is needed and why. **This field never contains the credential/secret value itself** — see the `input_request` section below. |
| `response_options` | `string[]`                                                                                     | only for `input_request` | The closed set of choices the user may pick from. It may legitimately be `[]` for kinds with no discrete choices (`credential`, `mfa`). |
| `expires_at`       | `number`                                                                                       | only for `input_request` | Unix epoch **milliseconds** after which the daemon considers this request expired, if no matching `input_response` arrives. It is a plain epoch-millis integer, not an ISO 8601 string, since JSON has no native timestamp type and this crate has no `chrono`/`time` dependency. |

- `ack`: the daemon acknowledges receipt of a `ClientMessage`. For
  example, the daemon received the prompt and handed it to the bridge.
  `text` is optional. When present, it may echo back what was
  acknowledged.
- `status`: a general daemon or connection status update for the iOS
  status panel, e.g. "connected to holo-desktop-cli", "broadcast
  started", or the initial "control channel ready" greeting. The daemon
  also uses it for the expiry-to-safe-pause notification described under
  `input_request` below.
- `task_progress`: an in-progress update from the `holo-desktop-cli`
  bridge. The bridge sends it while processing a `prompt` or `voice_transcript`. See
  README's "Holo bridge" section: "Progress/results are relayed back over
  the control channel".
- `task_done`: the terminal lifecycle signal for one task. The turn
  named by the envelope's `task_id` reached `status`
  (`completed`/`failed`/`canceled`), with optional human-readable detail
  in `text`. The schema added `task_done` additively, so a client's task
  controls have a reliable end-of-task signal to key off. Previously, a
  terminal folded into free-text `status`/`error` lines. Note the pause
  interaction: pausing a task cancels its running turn (see `pause`
  above). Because of this, a `task_done` with `"canceled"` arrives even
  for a task the user considers merely paused.
- `task_active`: the daemon sends this right after the greeting on a
  reconnect. It sends the message when an earlier task is still live. The
  task can be running, parked (`paused`), or have prompts `queued` behind it.
  It exists so a reconnecting client can **restore its task-control
  surface** (the Pause/Stop pill) from a structured signal. This avoids
  keying UI state off a free-text `status` line. A parked task is
  not otherwise "busy." This is the only reconnect signal for a paused task. This message type is additive and
  optional. Older clients that do not recognize `task_active` fall back
  to their generic "unrecognized control event" handling. They do not
  restore the pill.
- `error`: something failed. Causes include:
  - A bad envelope or malformed payload.
  - An envelope rejection under the preceding rules.
  - A bridge process crash or capture failure. `text` should contain enough detail to
  show the user; it does not need a full stack trace.
- `auth_rejected`: the daemon sends this instead of the normal greeting
  when an unrecognized device fails the PIN gate. Causes include:
  - A wrong or missing PIN.
  - A malformed first message.
  - A closed connection before PIN submission.

  See `mac-daemon/PAIRING.md`. The
  daemon sends this message **unwrapped**, not inside a `TaskEnvelope`. No
  `session_id` exists at this point. See "The one exception" above. The daemon closes the connection immediately after
  it sends this message. A client that receives it should return to a
  pairing/PIN-entry UI, rather than treating it like a generic `error`.
- `current_ticket`: the daemon sends its current node-id-only ticket after the
  greeting. The frame travels over the authenticated connection. The client
  uses it to refresh a stale saved default after daemon identity rotation.
- `clarify_questions`: returns the questions for `clarify_request`. Each item
  has a `question` and an `options` array. An empty array tells the client to
  send the original prompt without a clarification panel.
- `secure_input_state`: reports transitions into and out of macOS secure input.
  The client uses `active: true` to explain why ScreenCaptureKit hides a login,
  lock-screen, Keychain, or password field.
- `tinfoil_verification`: carries the ground truth from the same official
  origin-bound Tinfoil client that performs Holoiroh cloud egress. The daemon
  sends it after authentication only when attestation succeeded. `host` is the
  exact pinned HTTPS origin. `ground_truth` includes the release digest and
  code and enclave measurements. It also includes Transport Layer Security
  (TLS) and Hybrid Public Key Encryption (HPKE) public keys. It includes both
  fingerprints.
  The bearer application programming interface (API) key is not part of this message and never leaves the daemon.
  Absence means that Tinfoil is disabled or attestation failed. The client must
  not use the hosted generic Verification Center as proof for this connection.
- `input_request`: asks the user for structured input the agent cannot
  proceed without it (Project Aro PRD row P0-14). See the dedicated
  section below. This is the most involved variant on the wire, and it
  has security properties the others do not.

### Tinfoil-backed replies: `document_processed`/`document_process_failed`, `image_analyzed`/`image_analysis_failed`, `audio_transcribed`/`audio_transcription_failed`, `speech_ready`/`speech_failed`, `plan_ready`/`plan_failed`

These are the success/failure reply pairs for the five `ClientMessage`
types above (see that section for the request shapes). Each reply
carries the `request_id` it answers, so the client can correlate it
against multiple in-flight requests. Unlike `clarify_questions`, these
are not assumed to be at most one at a time.

```json
{ "type": "document_processed", "request_id": "...", "markdown": "# Notes\n..." }
{ "type": "document_process_failed", "request_id": "...", "error": "file too large" }
{ "type": "image_analyzed", "request_id": "...", "text": "A login form." }
{ "type": "image_analysis_failed", "request_id": "...", "error": "..." }
{ "type": "audio_transcribed", "request_id": "...", "text": "hello there" }
{ "type": "audio_transcription_failed", "request_id": "...", "error": "..." }
{ "type": "speech_ready", "request_id": "...", "audio_data_base64": "..." }
{ "type": "speech_failed", "request_id": "...", "error": "..." }
{ "type": "plan_ready", "request_id": "...", "steps": ["Desktop action: open Mail and find the last email from Sam", "Desktop action: reply and archive"] }
{ "type": "plan_failed", "request_id": "...", "error": "..." }
```

### `input_request` / `input_response`

`input_request` (server → client) is how the daemon pauses a running turn
to ask the user something. Its `kind` is one of:

| `kind`                      | Meaning | Typical `response_options` |
|------------------------------|---------|------------------------------|
| `credential`                 | The agent needs a credential (password, API key, secret token, etc.). | `[]`. No discrete choices; see "Credentials never travel on this channel" below. |
| `mfa`                        | The agent needs a multi-factor authentication code or approval. | `[]`, same reasoning as `credential`. |
| `ambiguous_choice`           | The agent found more than one plausible way to proceed. | The candidate options, e.g. `["Work calendar", "Personal calendar"]`. |
| `missing_info`                | The agent is missing information it cannot infer or safely guess. | Often `[]` (an open question like "which recipient email?"), but may list options when the answer is itself a closed set. |
| `sensitive_access_consent`   | The next step touches something sensitive (a payment, a destructive action, private data) and needs explicit consent first. | Typically a yes/no pair, e.g. `["Yes, proceed", "No, cancel"]`. |

**Live producer (PRD §9 sensitive-app gate).** As of this revision, the
daemon actually emits `sensitive_access_consent` requests. A per-turn
watchdog polls the Mac's frontmost application while the agent acts. It
classifies the application's bundle id against the user-editable class-5
category config (`~/.holoiroh/sensitive_categories.toml` — see
`mac-daemon/src/sensitive_categories.rs`).

An `always_ask` category match pauses the turn, using the same
park-the-turn mechanics as the wire `pause`. It sends an `input_request`
with `response_options` `["Allow once", "Stop task"]`. `Allow once`
resumes the turn and covers that category for the rest of the SAME task.
Anything else, or expiry after 120s, leaves the task safely
stopped/paused. A `hard_block` category cancels the turn outright, with a
`status` explaining why. An `always_allow` category proceeds silently.

**Credentials never travel on this channel — hard requirement, not a
convention.** `input_request`'s `context`/`response_options` fields are
metadata. They describe what is needed and why. Examples include:

- "Holo needs your GitHub personal access token to push this branch."
- "Enter the 6-digit code from your authenticator app."

These fields never carry a credential, secret, or MFA code. The Rust implementation
(`ServerMessage::input_request` in `control_channel.rs`) has no
parameter through which a caller could thread one in.

This implements the Project Aro PRD requirement. Credential characters never
appear in logs, screenshots, or task events. For `credential` and `mfa`,
`input_request` only announces that manual entry is necessary. The
actual value is designed to flow over a **separate `manual_input`
channel**. This channel is not part of this wire schema, and this
control channel does not implement it at all. The design prevents the value from reaching the model or agent context. The
large language model (LLM) that drives `holo-desktop-cli` never receives the
raw credential. Only a
human-operated, out-of-band path sees it.

The project tracks building that `manual_input` channel as its own,
separate PRD row. This document guarantees one property. `input_request` and `input_response`
cannot leak credentials into control channel messages, task-event logs, or
screenshots.

`input_response` (client → server, see `ClientMessage` above) is the
user's answer for the `ambiguous_choice`/`missing_info`/
`sensitive_access_consent` kinds. It is a structured selection among
`response_options`. For `credential`/`mfa` kinds, `input_request` only
ever announces that manual entry is needed. The user provides the actual
secret out of band, via the separate `manual_input` channel, never as an
`input_response`.

**Expiry-to-safe-pause.** If no `input_response` arrives before
`expires_at`, the daemon does **not** treat this as a failure. It emits a `status` message, never an `error`. The `text` says that the task
paused safely and waits for input. For example:

```json
{ "type": "status", "text": "input request d290f1ee-6c54-4b01-90e6-d701748f0851 expired with no response -- task safely paused, waiting for input" }
```

The daemon then clears the pending request, connection-side. It treats a later `input_response` for that `request_id` as unmatched. It
does not restore the expired request. See `input_response` under
`ClientMessage` above.

**At most one `input_request` is outstanding per connection at a
time.** This matches the control channel's existing single-active-turn
concurrency model. One `prompt`/`voice_transcript` turn runs at a time
per connection, with others queued (see the "Holo bridge" section of
`README.md`). An `input_request` pauses that one in-flight turn. Because
of this, no scenario today needs a second `input_request` before the
first is answered or expires.

## Typed action approval

The protocol defines `approval_request` for one exact proposed action.
The current Holo bridge does not issue this message on its live action path.
All fields are required.

```json
{
  "type": "approval_request",
  "approval_id": "f6df06b4-bd84-4f84-a817-adf58a69a2ee",
  "action_id": "action-42",
  "proposal_digest": "0000000000000000000000000000000000000000000000000000000000000000",
  "run_id": "run-9",
  "task_id": "task-7",
  "risk": "critical",
  "effect": {
    "app": "Mail",
    "target": "recipient@example.com",
    "material": "Send the reviewed message"
  },
  "before_state_digest": "1111111111111111111111111111111111111111111111111111111111111111",
  "expires_at": 1784349165135
}
```

`risk` is `low`, `medium`, `high`, or `critical`. The daemon assigns it.
It does not accept a model-supplied risk value. `effect` is structured data.
The app displays its exact `app`, `target`, and `material` values. It does not
build a response from text on the screen.

The app answers with a distinct `approval_response`. It does not use
`input_response`.

```json
{
  "type": "approval_response",
  "approval_id": "f6df06b4-bd84-4f84-a817-adf58a69a2ee",
  "action_id": "action-42",
  "proposal_digest": "0000000000000000000000000000000000000000000000000000000000000000",
  "decision": "approve"
}
```

`decision` is `approve`, `deny`, or `cancel`. The response repeats no effect,
risk, run, task, state, or expiry data. The signed `TaskEnvelope` supplies the
session and task metadata.

The daemon applies these invariants:

- It verifies the signed envelope before it handles `approval_response`.
- It computes `proposal_digest` as domain-separated SHA-256 over length-prefixed
  canonical fields. These fields include goal and intent digest, observation ID
  and digest, run/task/action IDs, full target preconditions, and exact action
  parameters. Draft text contributes only its SHA-256 digest.
- It creates `approval_id` with a cryptographic random source.
- It limits approval lifetime to 60 seconds.
- It stores binding metadata and digests. It does not store raw effect or
  secret content.
- It requires exact session, task, action, proposal, and before-state bindings.
- It consumes a matching response once. It rejects replay, expiry, denial as
  approval, canceled tasks, and stale before-state.
- Stop, pause, redirect, disconnect, and terminal task events invalidate
  matching pending approvals. Invalidation does not consume or execute them.
- The control channel routes verified responses into the daemon-shared approval
  store. Only the typed executor consumes them.
- The typed executor captures fresh target state before it issues a commitment
  approval. It captures two independent states after the signed response and
  immediately before atomic approval consumption and execution. Sensitive-target
  mutation remains unsupported and issues no approval.
- The executor appends metadata-only proposal, policy, approval, precondition,
  receipt, and terminal-outcome records. It never logs draft text, effect
  material, target title/value content, credentials, or observation content.

The daemon-owned typed executor is the only autonomous path for structured
actions. It accepts only its closed semantic action union and primitive adapter.
It never routes a typed action to opaque Holo. Denial, cancel, expiry, replay,
stale state, unresolved targets, credentials, and unsupported actions execute
nothing. Planner integration is not implemented.

The current Holo runtime remains an explicit unsafe compatibility backend. It
owns an opaque action stream and has no typed before-action callback. A typed
failure never falls back to this backend.

The app fails closed when any required field, risk value, effect field, or
message type is malformed. It requires identifiers of 1 through 128 printable
ASCII bytes. It requires both digests to contain exactly 64 lowercase
hexadecimal characters. It bounds `app` to 128 bytes, `target` to 512 bytes,
and `material` to 1024 bytes. It requires `expires_at` to be in the future when
it decodes the request. Approve, deny, explicit cancel, and sheet dismissal each
send one signed typed response. Sheet dismissal sends `cancel`. The app clears
its retained request before it sends, so a dismissal callback cannot send a
second response. It does not show approval controls for a rejected frame.

## Serialization

Both `ClientMessage` and `ServerMessage` are tagged, internally-tagged
enums keyed on `type` (`#[serde(tag = "type", rename_all =
"snake_case")]`). This matches the wire examples above exactly. There is
no separate wrapper object at the payload level. `type` and `text` (or
`pin`) are sibling fields of one flat JSON object per payload.

`text` is `Option<String>`. The serializer omits it from the JSON when
it is `None` (`#[serde(skip_serializing_if = "Option::is_none")]`),
rather than emitting it as `"text": null`. Because of this, `stop` and
`ack` payloads serialize as `{"type":"stop"}` / `{"type":"ack"}` with no
`text` key at all. The `TaskEnvelope<T>` wrapper around this payload is
a plain (non-tagged) struct. `payload` is just a normal field holding
the tagged enum above. See "Envelope" for its own field table.

`input_request`'s fields (`request_id`, `kind`, `context`,
`response_options`, `expires_at`) and `input_response`'s fields
(`request_id`, `selected_option`) are **not** optional. Unlike `text`, each field is always present in the serialized JSON. They are
plain `String`, `Vec<String>`, or `u64` fields without `skip_serializing_if`. This
is because `input_request`/`input_response` are structured messages,
where a missing field would be ambiguous rather than "absent detail."

An empty `response_options` (`[]`) is a normal, expected value for kinds
with no discrete choices (see the `input_request` section above). The
serializer never omits it entirely, so a client can always index into it
without a null-check. `kind` itself serializes as a bare snake_case
string (`InputRequestKind`'s own `#[serde(rename_all = "snake_case")]`,
with no nested tag). It sits directly as `input_request`'s `kind` field
value.

## Error handling on malformed input

Post-auth malformed JSON/shell, signature failure, malformed typed payload,
message-type mismatch, or state rejection is never dispatched. The daemon
returns a bounded signed error when it can keep the stream open. Oversized or
invalid-UTF-8 frames close the stream after bounded diagnostics. The iOS bridge fails the connection after any invalid daemon envelope or frame.
It reports a bounded polling error. It never exposes the rejected line to Swift.

## Known gaps

- **Five of the six PRD-named streams are not implemented.** See
  "Project Aro PRD context" above. This document, and this codebase,
  cover `control` only.

## Coordinated release requirement

Signature enforcement has no unsigned compatibility flag. The app, bridge, and daemon must ship together. The new daemon rejects unsigned
envelopes from an old client. The new bridge rejects unsigned greetings from
an old daemon before it reports connection success.

## Envelope versioning

This project bumps `PROTOCOL_VERSION`
(`control_channel::PROTOCOL_VERSION`, currently `1`) only on a
deliberate, coordinated change to the envelope shape itself. Examples
include adding, removing, or retyping a top-level envelope field. This
version is independent of the crate's own `Cargo.toml` version. It is
also independent of changes to `ClientMessage`/`ServerMessage`'s own
payload shape. Those changes are additive-only per "Future extension"
below, and do not need an envelope version bump.

## Future extension

This schema intentionally started minimal, per the task scope:
`prompt`/`voice_transcript`/`stop` and
`ack`/`status`/`error`/`task_progress`. The following additions extended it:

- `pin` and `auth_rejected` for pairing.
- `input_request` and `input_response` for structured user decisions.
- `approval_request` and `approval_response` for typed action approval.
- Clarification messages, task lifecycle, and secure input state.
- Tinfoil feature request and reply pairs.
- `current_ticket` and `tinfoil_verification`.

Going forward, fields are additive-only. A future revision may add new
optional fields or new `type` variants. Existing field names/types and
existing `type` values must not change meaning. This lets a client built
against an older revision of this document degrade gracefully. After client-side implementation, it should ignore or log unknown `type`
values. It should not treat them as a hard parse error. This
document versions the envelope shape itself (`TaskEnvelope<T>`'s own
fields) separately — see "Envelope versioning" above.

`input_request`/`input_response` themselves follow this same additive
policy. A client that predates this revision simply never recognizes
`input_request`. It falls back to ignoring or logging it, per the policy
above, until it is updated.

This project designed a **separate `manual_input` channel** for real
credential/secret entry. It is not part of this document's schema at all
(see the `input_request`/`input_response` section above for why). The
project tracks it as its own future PRD row, not an extension of this
NDJSON control channel.
