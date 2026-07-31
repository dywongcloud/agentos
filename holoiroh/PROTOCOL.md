# HoloIroh control-channel protocol

This document defines the JSON message schema exchanged between the iOS
app (`ios/`) and the Mac daemon (`mac-daemon/`) over the **control
channel**. The control channel is a second, bidirectional logical stream.
It runs alongside the `iroh-live` media broadcast, on the same `iroh`
`Endpoint`. See `README.md`'s "Control channel" section and "Why iroh /
iroh-live specifically" section for the architecture rationale.

This document is the source of truth for the wire schema. The Rust types
that implement the schema via `serde` (`ClientMessage`, `ServerMessage`,
`TaskEnvelope<T>`) live in the `holoiroh-wire` crate
(`holoiroh-wire/src/lib.rs`). This lets both `mac-daemon` and `ios-bridge`
share one definition instead of duplicating it. `ios-bridge` is the iOS
FFI crate. It must cross-compile to `aarch64-apple-ios`. It cannot depend
on `mac-daemon`'s macOS-only `holo_bridge`/`audit_log` modules.
`mac-daemon/src/control_channel.rs` re-exports them at the same
`control_channel::{ClientMessage, ServerMessage, TaskEnvelope, ...}`
paths. It also owns the connection-handling logic that *uses* this
schema: the `iroh` `ProtocolHandler` impl, the PIN/allowlist auth gate,
and per-connection sequence state. Any change here requires a matching
change in `holoiroh-wire/src/lib.rs`, and eventually in the Swift client.

## Project Aro PRD context: six logical streams, one implemented

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

- **ALPN:** `holoiroh/control/1`. This is a dedicated ALPN string. It is
  registered on the same `iroh::Endpoint`/`iroh::protocol::Router` that
  also serves `iroh-live`'s MoQ (`iroh-moq`) and, if enabled, gossip
  ALPNs. See `iroh_live::Live::register_protocols`, which this project's
  `ControlChannel::register_protocols` mirrors. The control channel uses a
  distinct ALPN rather than a second stream multiplexed inside the media
  `Connection`. Because of this, the control channel is its own
  `iroh::endpoint::Connection`. It is still dialed to the *same peer*
  (`EndpointId`) as the media broadcast, over the *same*
  `iroh::Endpoint`. This gives it identical NAT-punched path/relay
  fallback, and identical connection-lifecycle and reconnect story. This
  is what "a second logical stream on the same iroh QUIC connection"
  means in `iroh`'s connection-per-ALPN model.
- **Stream:** one bidirectional QUIC stream per control-channel
  connection. The dial side opens it via `Connection::open_bi()`. The
  accept side accepts it via `Connection::accept_bi()`, inside the
  `ProtocolHandler::accept` callback.
- **Framing:** newline-delimited JSON (NDJSON). Each message is a single
  JSON object serialized on one line, terminated by `\n`. The receiver
  reads with a line-buffered reader
  (`tokio::io::AsyncBufReadExt::read_line`). It deserializes each line
  independently. This keeps framing trivial (no length-prefix codec
  needed) since control messages are small and human-inspectable in a
  packet capture or log.
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

The sender wraps every control-channel message in a `TaskEnvelope`,
**except the pre-session PIN handshake** (see "The one exception: the PIN
handshake is unwrapped" below). This matches the Project Aro PRD's
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
  "signature": null
}
```

| Field              | Type                | Required | Meaning |
|--------------------|---------------------|----------|---------|
| `protocol_version` | `u32`                | yes | This is the envelope schema's version. It is currently always `1` (`control_channel::PROTOCOL_VERSION`). The daemon logs a mismatch instead of rejecting it, since only one version exists yet. |
| `message_id`       | `string` (uuid v4)   | yes | Each message has a unique `message_id`, minted fresh by whichever side sends it. The daemon uses it for duplicate-message rejection (see "Rejection rules" below). |
| `session_id`       | `string` (uuid v4)   | yes | The daemon mints `session_id` once per accepted `iroh` connection (see "Session lifecycle" below). It stays stable for that connection's lifetime. Every envelope on a given connection carries the same `session_id`, in either direction. |
| `task_id`          | `string` \| `null`   | no  | `task_id` correlates an envelope with a specific bridge turn. A turn is a `prompt`/`voice_transcript`/`stop` and the `ack`/`status`/`task_progress`/`error` replies it produces. It is `null` or omitted for envelopes with no turn to correlate to, such as the initial greeting or a reconnect status update. See "task_id threading" below. |
| `message_type`     | `string`             | yes | `message_type` mirrors `payload`'s own `type` discriminant (e.g. `"prompt"`, `"ack"`) as a top-level, envelope-inspectable field. This field is deliberately redundant with `payload.type` rather than unified with it. This redundancy lets a reader inspect the envelope's framing without deserializing into a concrete payload type first. |
| `sent_at`          | `u64` (unix ms)      | yes | The time when the sender constructed this envelope. |
| `expires_at`       | `u64` (unix ms)      | yes | The receiver rejects this envelope if it arrives after this instant. `TaskEnvelope::new`/`wrap` default `expires_at` to `sent_at + 30_000` (30s) when they construct the envelope; see "Rejection rules". |
| `sequence_number`  | `u64`                | yes | `sequence_number` must strictly increase per `session_id`, per direction (see "Rejection rules" and "Two independent sequences, per direction" below). It starts at `0` for the first envelope either side sends on a fresh connection. |
| `payload`          | `ClientMessage` \| `ServerMessage` | yes | The actual message content. It has exactly the `{type, text?}` (or `{type, pin}`) shape documented below in "`ClientMessage`"/"`ServerMessage`". Envelope-wrapping does not change this shape. |
| `signature`        | `string` \| `null`   | no  | This field is present on the wire per the PRD schema. **The codebase does not cryptographically verify it as of this writing** — see "Known gaps" below. It is always `null`/omitted on envelopes this daemon constructs. |

The serializer omits `task_id` and `signature` from the JSON when they are
absent (`#[serde(skip_serializing_if = "Option::is_none")]`). It does not
emit them as `"task_id": null`. `ServerMessage.text` already used this same
convention before this schema existed.

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

The daemon mints a `session_id` (`uuid::Uuid::new_v4()`) once per accepted
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

This matches how the protocol scopes the envelope: "per `session_id` per
direction." The envelope does not use one global ordering across both
directions of a connection.

### task_id threading

- An inbound `ClientMessage` envelope's `task_id`, if present, becomes the
  `request_id` the daemon uses internally when it forwards the message to
  `holo_bridge`. A client that wants to correlate a
  `prompt`/`voice_transcript`/`stop` with its resulting
  `ack`/`task_progress`/`error` replies should set `task_id` itself. The
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

The daemon validates every inbound `TaskEnvelope` against three rules, in
this order, **before** it even parses the `payload` as a `ClientMessage`.
This applies to every message *except* the bare `pin` handshake (see
above):

1. **Expiry.** If the receiving side's current time is strictly greater
   than `expires_at`, the receiver rejects the envelope. Exactly `now ==
   expires_at` is **not** expired; only strictly-after counts as expired.
   `TaskEnvelope::new`/`wrap` stamp a default 30-second window
   (`expires_at = sent_at + 30_000`). A sender that wants a different
   window constructs the struct directly with an explicit `expires_at`.
2. **Duplicate `message_id`.** The daemon keeps an in-memory
   (`std::collections::HashSet<String>`) set of every `message_id` already
   seen on the current connection. The daemon rejects a repeated
   `message_id`, even with a legitimately-advanced `sequence_number`. This
   set is **per-connection** and not persisted. It starts empty on every
   fresh connection, including a reconnect from the same, already-paired
   device.
3. **`sequence_number` monotonicity.** The daemon tracks the last accepted
   inbound `sequence_number` for the current connection. A new envelope's
   `sequence_number` must be **strictly greater** than that last-accepted
   value. The daemon rejects an exact repeat or a lower number. Gaps are
   fine: for example, the daemon accepts a jump from `0` straight to
   `100`. The daemon rejects only non-increasing values.

The daemon **never forwards a rejected envelope to `holo_bridge`**. It
replies with `{"type":"error","text":"envelope rejected: <reason>"}`
(envelope-wrapped, echoing whatever `task_id` the rejected envelope
carried). It then continues reading the next line. This follows the
existing "malformed input is not a transport-level error" contract (see
"Error handling on malformed input" below, which this extends rather
than replaces).

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
{ "type": "remote_control", "event": { "action": "take_control" } }
{ "type": "remote_control", "event": { "action": "move", "x": 0.5, "y": 0.4 } }
{ "type": "remote_control", "event": { "action": "click", "x": 0.5, "y": 0.4, "button": "left", "count": 1 } }
{ "type": "remote_control", "event": { "action": "scroll", "x": 0.5, "y": 0.4, "dx": 0.0, "dy": -3.0 } }
{ "type": "remote_control", "event": { "action": "text", "text": "hello team" } }
{ "type": "remote_control", "event": { "action": "release_control" } }
```

| Field             | Type                                                                        | Required | Meaning |
|-------------------|------------------------------------------------------------------------------|----------|---------|
| `type`            | `"prompt"` \| `"voice_transcript"` \| `"stop"` \| `"pause"` \| `"resume"` \| `"redirect"` \| `"pin"` \| `"input_response"` \| `"remote_control"` | yes | Discriminant. |
| `event`           | `RemoteControlEvent`                                                          | only for `remote_control` | This field is the touch-derived action. It is itself tagged by `action` (see below). |
| `text`            | `string`                                                                    | only for `prompt` / `voice_transcript` / `redirect` | The instruction text. The client always transcribes voice input before sending it. The wire format never carries raw audio (see README's "Prompts" section). |
| `pin`             | `string`                                                                    | only for `pin` | The PIN value. The Mac terminal shows this PIN to the user out-of-band, alongside the ticket. The user types or scans it into the client. |
| `request_id`      | `string`                                                                    | only for `input_response` | This field echoes the `request_id` of the `input_request` it answers. |
| `selected_option` | `string`                                                                    | only for `input_response` | The user's chosen option. It must be one of the original `input_request.response_options`. |

- `prompt`: a typed text instruction. The daemon hands it to the
  `holo-desktop-cli` bridge as-is.
- `voice_transcript`: functionally identical to `prompt` on the wire,
  using the same `text` field. The daemon tags it separately so the
  daemon/UI can distinguish input modality for logging or UX purposes.
  For example, the status panel can show a mic icon, without
  re-deriving the modality from context.
- `stop`: the remote **kill-switch**. It cancels or interrupts agent work
  on the Mac. It is one variant with two forms:
  - **Global form** (`{"type":"stop"}`, `context_id` absent): stop
    *everything*. On the daemon this maps, via
    `control_channel::to_control_message`, to `ControlMessage::Stop` with
    `context_id: None`. `HoloControlBridge::handle_stop` handles this in
    order:
    1. It scoped-cancels the currently running turn via A2A
       `tasks/cancel`, using the daemon's own resolved `contextId`/`Task.id`
       (the client never has one).
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
    exactly that context. It uses the real `Task.id` when the target is
    the currently running turn; this is the only place a `Task.id` can
    come from. A context-id-only cancel returns JSON-RPC `-32603` against
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
  backend exposes **no pause RPC** over A2A; its own kill switch is
  pause-then-cancel (see `mac-daemon/src/holo_bridge/stop.rs`'s source
  notes). Because of this, the daemon implements pause as the only honest
  primitive available: it **cancels** the running turn. It uses scoped
  A2A `tasks/cancel` once the turn's `contextId` resolves, or else the
  graceful global `holo stop`. While it does this, it stashes the turn's
  original instruction text and `contextId`. The canceled turn still
  produces its normal `task_done` (`canceled`); a client showing a
  "paused" state should expect and tolerate that terminal. Pausing with
  nothing running, or pausing twice, is a polite `status` reply, never an
  error.
- `resume`: re-dispatches the parked turn on the **same** `contextId`, so
  the backend session's history carries the task forward from where it
  was interrupted. The resumed turn runs under the `resume` envelope's
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
  the matching `input_request.response_options` offered. This message
  deliberately has no free-text field, so a client cannot accidentally,
  or on purpose, put a password, PIN, or MFA code into it. `request_id`
  might not match a currently-outstanding `input_request` on this
  connection — the request may already be expired, already answered, or
  never sent. In that case, the daemon replies with a normal `error`
  event, and the connection stays open. This is not a transport-level
  failure. It matches this document's general "malformed input"
  philosophy below. See `input_request`'s own entry for the full
  contract, including why real credential/MFA/manual entry is **not**
  carried by this message at all.
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
  (`tinfoil_vision.rs`). The daemon redacts the image on-device
  (`privacy.rs`'s OCR + PII redaction) before it ever leaves the daemon.
- `transcribe_audio`: transcribes audio (`tinfoil_audio.rs`,
  `voxtral-small-24b`). **This message must only ever carry audio
  captured from the client's own microphone.** Never send system or
  speaker output, since it could contain other call participants'
  voices. This is an explicit opt-in alternative to the default
  on-device `voice_transcript` path (`VoiceTranscriber.swift`'s on-device
  Speech framework). Sending this message means audio leaves the device.
  `format` is advisory only. Tinfoil infers the real format from
  content.
- `request_speech`: synthesizes `text` as speech (`tinfoil_audio.rs`,
  `qwen3-tts`). It returns WAV bytes.
- `plan_task`: asks Tinfoil's `glm-5.2` (tool-calling) to break `goal`
  into an ordered step list for the user to review. **This message
  plans; it does not execute.** Turning a step into action still
  requires a separate `prompt`/`remote_control` message. This is already
  how it works today.

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
{ "type": "input_request", "request_id": "d290f1ee-6c54-4b01-90e6-d701748f0851", "kind": "ambiguous_choice", "context": "Two calendars match 'team standup' -- which one?", "response_options": ["Work calendar", "Personal calendar"], "expires_at": 1800000120000 }
```

| Field              | Type                                                                                          | Required | Meaning |
|--------------------|-------------------------------------------------------------------------------------------------|----------|---------|
| `type`             | `"ack"` \| `"status"` \| `"error"` \| `"task_progress"` \| `"task_done"` \| `"task_active"` \| `"auth_rejected"` \| `"input_request"` | yes | Discriminant. |
| `text`             | `string`                                                                                       | optional on `ack`/`status`/`error`/`task_progress`/`task_done`/`auth_rejected` | Human-readable detail. |
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
  bridge. The bridge sends it while carrying out a
  `prompt`/`voice_transcript` (per README's "Holo bridge" section:
  "Progress/results are relayed back over the control channel").
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
  **(re)connect**, when a task from before the connection drop is still
  live: running, parked (`paused`), or with prompts `queued` behind it.
  It exists so a reconnecting client can **restore its task-control
  surface** (the Pause/Stop pill) from a structured signal. This avoids
  keying UI state off a free-text `status` line. A parked task is
  not otherwise "busy," so this is the only reconnect signal that
  surfaces a *paused* task at all. This message type is additive and
  optional. Older clients that do not recognize `task_active` fall back
  to their generic "unrecognized control event" handling, and simply do
  not restore the pill.
- `error`: something failed. Causes include a bad envelope, a malformed
  payload, an envelope rejected per the rules above, a bridge process
  crash, or a capture failure. `text` should contain enough detail to
  show the user; it does not need a full stack trace.
- `auth_rejected`: the daemon sends this instead of the normal greeting
  when an unrecognized device fails the PIN gate. Causes include a wrong
  or missing PIN, a malformed first message, or a connection closed
  before the client presents one; see `mac-daemon/PAIRING.md`. The
  daemon sends this message **unwrapped**, not inside a `TaskEnvelope`,
  since no `session_id` exists yet at this point (see "The one
  exception" above). The daemon closes the connection immediately after
  it sends this message. A client that receives it should return to a
  pairing/PIN-entry UI, rather than treating it like a generic `error`.
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
metadata. They describe *what* is needed and *why*, for example "Holo
needs your GitHub personal access token to push this branch" or "Enter
the 6-digit code from your authenticator app." These fields never carry
the actual credential/secret/MFA-code value. The Rust implementation
(`ServerMessage::input_request` in `control_channel.rs`) has no
parameter through which a caller could thread one in.

This directly implements the Project Aro PRD's requirement that
"credential characters are never logged, never included in screenshots,
never echoed in task events." For the `credential`/`mfa` kinds,
`input_request` only ever announces that manual entry is needed. The
actual value is designed to flow over a **separate `manual_input`
channel**. This channel is not part of this wire schema, and this
control channel does not implement it at all. The channel is
architected so the value never reaches the model/agent context: the LLM
driving `holo-desktop-cli` never sees the raw credential. Only a
human-operated, out-of-band path sees it.

The project tracks building that `manual_input` channel as its own,
separate PRD row. This document only guarantees that
`input_request`/`input_response` never become an accidental backdoor
for a credential to leak into a control-channel message, a task-event
log, or a screenshot.

`input_response` (client → server, see `ClientMessage` above) is the
user's answer for the `ambiguous_choice`/`missing_info`/
`sensitive_access_consent` kinds. It is a structured selection among
`response_options`. For `credential`/`mfa` kinds, `input_request` only
ever announces that manual entry is needed. The user provides the actual
secret out of band, via the separate `manual_input` channel, never as an
`input_response`.

**Expiry-to-safe-pause.** If no `input_response` arrives before
`expires_at`, the daemon does **not** treat this as a failure. It emits a
`status` message (never `error`) whose `text` says the task safely paused
and is waiting for input, e.g.:

```json
{ "type": "status", "text": "input request d290f1ee-6c54-4b01-90e6-d701748f0851 expired with no response -- task safely paused, waiting for input" }
```

The daemon then clears the pending request, connection-side. It treats a
later `input_response` for that same `request_id` as unmatched (see the
`input_response` entry under `ClientMessage` above), rather than
resurrecting the expired request.

**At most one `input_request` is outstanding per connection at a
time.** This matches the control channel's existing single-active-turn
concurrency model. One `prompt`/`voice_transcript` turn runs at a time
per connection, with others queued (see the "Holo bridge" section of
`README.md`). An `input_request` pauses that one in-flight turn. Because
of this, no scenario today needs a second `input_request` before the
first is answered or expires.

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
(`request_id`, `selected_option`) are **not** optional. Unlike `text`,
every one of these is always present in the serialized JSON: plain
`String`/`Vec<String>`/`u64` fields, with no `skip_serializing_if`. This
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

**None of the following failures are a transport-level error:**

- A line that fails to parse as valid JSON at the envelope level.
- A line that parses as an envelope, but is missing or mistypes a
  required framing field.
- A line that parses as a valid envelope, but whose `payload` fails to
  parse as `ClientMessage`.
- A line that parses fine at every level, but fails one of the three
  rejection rules above.

The connection and stream stay open in every case. The receiving side
logs the failure. On the daemon side, the daemon sends back an
envelope-wrapped `{"type": "error", "text": "..."}` describing which of
these categories the failure was. The exact text distinguishes
"malformed envelope", "malformed payload", and "envelope rejected:
<expiry|duplicate|sequence reason>" (see `control_channel.rs`'s
`ProtocolHandler::accept` for the exact wording). The daemon then
continues reading the next line.

Only stream/connection-level failures (EOF, reset, peer disconnect) end
the control-channel task. There is one case with no envelope to wrap a
reply in: a completely unparseable line arriving during the bare-PIN
pre-session window. `authenticate`'s own gate handles this case. It
rejects the whole connection, rather than replying inline (see "The one
exception" above).

## Known gaps

- **`signature` is not cryptographically verified.** The field exists on
  the wire: present per the PRD schema, always `null`/omitted on
  envelopes this daemon constructs. Nothing in this codebase verifies it
  against anything, since there is no signing keypair/identity
  infrastructure here yet. The `iroh` node keypair authenticates the
  *transport*, meaning who the connection is to. It does not
  authenticate individual envelopes. A genuine envelope-signing scheme —
  what key, over which fields, verified where — is separate, unbuilt
  work.
- **Five of the six PRD-named streams are not implemented.** See
  "Project Aro PRD context" above. This document, and this codebase,
  cover `control` only.
- **`protocol_version` mismatch is not hard-enforced.** The daemon logs
  a mismatch instead of rejecting it, since exactly one version (`1`)
  exists as of this writing. A real enforcement policy — reject unknown
  versions? negotiate? — is unbuilt work for whenever a second version
  actually exists.

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
`ack`/`status`/`error`/`task_progress`. It grew additively since then:
`pin`/`auth_rejected` for pairing, and `input_request`/`input_response`
for Project Aro PRD row P0-14.

Going forward, fields are additive-only. A future revision may add new
optional fields or new `type` variants. Existing field names/types and
existing `type` values must not change meaning. This lets a client built
against an older revision of this document degrade gracefully. Once the
client-side implementation exists, it should ignore or log unknown
`type` values, rather than treat them as a hard parse error. This
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
