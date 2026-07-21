# HoloIroh control-channel protocol

Defines the JSON message schema exchanged between the iOS app (`ios/`) and
the Mac daemon (`mac-daemon/`) over the **control channel**: a second,
bidirectional logical stream running alongside the `iroh-live` media
broadcast, on the same `iroh` `Endpoint` (see `README.md`'s "Control
channel" section and "Why iroh / iroh-live specifically" for the
architecture rationale).

This document is the source of truth for the wire schema. The Rust types
that implement it via `serde` (`ClientMessage`, `ServerMessage`,
`TaskEnvelope<T>`) live in the `holoiroh-wire` crate (`holoiroh-wire/src/lib.rs`)
so that both `mac-daemon` and `ios-bridge` (the iOS FFI crate, which must
cross-compile to `aarch64-apple-ios` and cannot depend on `mac-daemon`'s
macOS-only `holo_bridge`/`audit_log` modules) can share one definition
instead of duplicating it. `mac-daemon/src/control_channel.rs`
re-exports them at the same `control_channel::{ClientMessage, ServerMessage,
TaskEnvelope, ...}` paths (and owns the connection-handling logic that
*uses* this schema: the `iroh` `ProtocolHandler` impl, the PIN/allowlist
auth gate, per-connection sequence state) — any change here must be
mirrored in `holoiroh-wire/src/lib.rs`, and eventually in the Swift client.

## Project Aro PRD context: six logical streams, one implemented

The Project Aro PRD names **six** logical streams for this system: `control`,
`pairing`, `media`, `manual_input`, `snapshot_fallback`, and `telemetry`.
**Only `control` is implemented in this codebase today** -- this document
covers that one stream exclusively. The other five are PRD-tracked, not-yet-
built work; nothing below should be read as describing them. `pairing`'s
narrower incremental precursor (PIN + device allowlist, no envelope wrapping
of its own -- see "Envelope" below) is real and documented in
[`mac-daemon/PAIRING.md`](./mac-daemon/PAIRING.md), but that is a distinct,
already-existing mechanism layered *underneath* the control stream's auth
gate, not the PRD's `pairing` stream itself.

## Transport

- **ALPN:** `holoiroh/control/1` — a dedicated ALPN string registered on the
  same `iroh::Endpoint`/`iroh::protocol::Router` that also serves
  `iroh-live`'s MoQ (`iroh-moq`) and, if enabled, gossip ALPNs (see
  `iroh_live::Live::register_protocols`, which this project's
  `ControlChannel::register_protocols` mirrors). Because it's a distinct
  ALPN rather than a second stream multiplexed inside the media
  `Connection`, the control channel is its own `iroh::endpoint::Connection`
  — but it is dialed to the *same peer* (`EndpointId`) as the media
  broadcast, over the *same* `iroh::Endpoint` (identical NAT-punched
  path/relay fallback, identical connection-lifecycle and reconnect story),
  which is what "a second logical stream on the same iroh QUIC connection"
  means in `iroh`'s connection-per-ALPN model.
- **Stream:** one bidirectional QUIC stream per control-channel connection,
  opened via `Connection::open_bi()` (dial side) and accepted via
  `Connection::accept_bi()` (accept side, inside the `ProtocolHandler::accept`
  callback).
- **Framing:** newline-delimited JSON (NDJSON). Each message is a single
  JSON object serialized on one line, terminated by `\n`. The receiver reads
  with a line-buffered reader (`tokio::io::AsyncBufReadExt::read_line`) and
  deserializes each line independently. This keeps framing trivial (no
  length-prefix codec needed) since control messages are small and
  human-inspectable in a packet capture or log.
- **Direction:** the iOS app is the dial side and sends `ClientMessage`
  values; the Mac daemon is the accept side and sends `ServerMessage`
  values. Both directions share the same bidirectional stream (`SendStream`
  + `RecvStream` pair from the one `accept_bi()`/`open_bi()` call) — the
  daemon reads `ClientMessage` off `RecvStream` while writing
  `ServerMessage` onto `SendStream`, and vice versa on the iOS side.

## Envelope

Every control-channel message **except the pre-session PIN handshake** (see
"The one exception: the PIN handshake is unwrapped" below) is wrapped in a
`TaskEnvelope`, matching the Project Aro PRD's authoritative task-envelope
shape:

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
| `protocol_version` | `u32`                | yes | This envelope schema's version. Currently always `1` (`control_channel::PROTOCOL_VERSION`). A mismatch is logged, not hard-rejected, since only one version exists yet. |
| `message_id`       | `string` (uuid v4)   | yes | Unique per message, minted fresh by whichever side sends it. Used for duplicate-message rejection (see "Rejection rules" below). |
| `session_id`       | `string` (uuid v4)   | yes | Minted once per accepted `iroh` connection (see "Session lifecycle" below) and stable for that connection's lifetime. Every envelope either direction on a given connection carries the same `session_id`. |
| `task_id`          | `string` \| `null`   | no  | Correlates an envelope with a specific bridge turn (a `prompt`/`voice_transcript`/`stop` and the `ack`/`status`/`task_progress`/`error` replies it produces). `null`/omitted for envelopes with no turn to correlate to (the initial greeting, a reconnect status update). See "task_id threading" below. |
| `message_type`     | `string`             | yes | Mirrors `payload`'s own `type` discriminant (e.g. `"prompt"`, `"ack"`) as a top-level, envelope-inspectable field — deliberately redundant with `payload.type` rather than the two being unified, so the envelope's framing is readable without deserializing into a concrete payload type first. |
| `sent_at`          | `u64` (unix ms)      | yes | When this envelope was constructed. |
| `expires_at`       | `u64` (unix ms)      | yes | This envelope is rejected if received after this instant. Defaults to `sent_at + 30_000` (30s) when constructed via `TaskEnvelope::new`/`wrap`; see "Rejection rules". |
| `sequence_number`  | `u64`                | yes | Must strictly increase per `session_id`, per direction (see "Rejection rules" and "Two independent sequences, per direction" below). Starts at `0` for the first envelope either side sends on a fresh connection. |
| `payload`          | `ClientMessage` \| `ServerMessage` | yes | The actual message content — exactly the `{type, text?}` (or `{type, pin}`) shape documented below in "`ClientMessage`"/"`ServerMessage`", unchanged by envelope-wrapping. |
| `signature`        | `string` \| `null`   | no  | Present on the wire per the PRD schema. **Not cryptographically verified in this codebase as of this writing** — see "Known gaps" below. Always `null`/omitted on envelopes this daemon constructs. |

`task_id` and `signature` are omitted from the serialized JSON when absent
(`#[serde(skip_serializing_if = "Option::is_none")]`), not emitted as
`"task_id": null` — same convention `ServerMessage.text` already used before
this schema existed.

### The one exception: the PIN handshake is unwrapped

`session_id` does not exist yet when an unrecognized device's `pin` message
is sent, nor when the daemon's `auth_rejected` reply (if the PIN is
wrong/missing) is sent back — a `session_id` is only minted **after** the
auth gate passes (see "Session lifecycle" below). Wrapping a message in a
`session_id`-bearing envelope before one exists would require either a
placeholder/empty `session_id` (misleading — it isn't really *this*
connection's session) or delaying session-minting further (which would
break the auth gate's existing contract of reading exactly one line before
anything else happens). Given that, the choice made here is: **the `pin`
`ClientMessage` and the `auth_rejected` `ServerMessage` are sent as bare,
unwrapped JSON**, exactly the pre-envelope wire shape (`{"type":"pin","pin":"..."}`,
`{"type":"auth_rejected","text":"..."}`). Every message from the
"control channel ready" greeting onward — which is the first message sent
*after* a `session_id` is minted — is envelope-wrapped. This is a real
architectural boundary in `control_channel.rs`, not an oversight: search for
`ControlChannel::authenticate` to see the exact point where the switch
happens.

Already-allowlisted devices (and daemons run with `--no-pin-auth`) never
send/receive `pin`/`auth_rejected` at all — for them, the very first message
on the stream is the envelope-wrapped greeting.

### Session lifecycle

A `session_id` is minted (`uuid::Uuid::new_v4()`) once per accepted `iroh`
connection, immediately after the PIN/allowlist auth gate passes (or is
skipped because the device is already allowlisted / `--no-pin-auth` is set).
It has no relationship to the persisted device allowlist or to
`iroh`'s own connection/node identity — a device reconnecting (even the same
allowlisted device, even immediately after a clean disconnect) gets a fresh
`session_id` on its next connection. Nothing about `session_id` (or the
sequence-number/seen-`message_id` state scoped to it — see "Rejection
rules") is persisted across connections or daemon restarts.

### Two independent sequences, per direction

`sequence_number` is **not** one shared counter for the whole connection —
each direction has its own, starting at `0` independently:

- The daemon's own outgoing `ServerMessage` envelopes are numbered `0, 1,
  2, ...` in the order the daemon sends them (greeting is always `0`).
- The client's incoming `ClientMessage` envelopes are validated against
  their own separately-tracked last-seen `sequence_number` — the daemon
  does not care what number the client used relative to what the daemon
  itself has sent.

This matches the envelope being scoped "per `session_id` per direction," not
a single global ordering across both directions of one connection.

### task_id threading

- An inbound `ClientMessage` envelope's `task_id` (if present) becomes the
  `request_id` the daemon uses internally when forwarding to `holo_bridge`
  — i.e., a client that wants to correlate a `prompt`/`voice_transcript`/
  `stop` with its resulting `ack`/`task_progress`/`error` replies should set
  `task_id` itself and expect it echoed back on every reply envelope for
  that turn.
- An inbound envelope that omits `task_id` gets a fresh `uuid` synthesized
  by the daemon (matching this daemon's pre-envelope behavior, where a
  `request_id` was always synthesized regardless of what the client sent)
  — and that synthesized id becomes the `task_id` on the reply envelopes for
  that turn, so a client can still observe which replies belong together
  even without providing its own id up front.
- The `pin` message and the initial "control channel ready" greeting/
  reconnect-status messages have no `task_id` (there is no bridge turn to
  correlate to yet).

## Rejection rules

Every inbound `TaskEnvelope` (i.e., every message *except* the bare `pin`
handshake — see above) is validated against three rules, in this order,
**before** its `payload` is even parsed as a `ClientMessage`:

1. **Expiry.** If the receiving side's current time is strictly greater
   than `expires_at`, the envelope is rejected. (Exactly `now == expires_at`
   is **not** expired — only strictly-after.) `TaskEnvelope::new`/`wrap`
   stamp a default 30-second window (`expires_at = sent_at + 30_000`); a
   sender wanting a different window constructs the struct directly with an
   explicit `expires_at`.
2. **Duplicate `message_id`.** The daemon keeps an in-memory
   (`std::collections::HashSet<String>`) set of every `message_id` already
   seen on the current connection. A repeated `message_id` — even with a
   legitimately-advanced `sequence_number` — is rejected. This set is
   **per-connection**, not persisted, and starts empty on every fresh
   connection (including a reconnect from the same, already-paired device).
3. **`sequence_number` monotonicity.** The daemon tracks the last accepted
   inbound `sequence_number` for the current connection. A new envelope's
   `sequence_number` must be **strictly greater** than that last-accepted
   value — an exact repeat or a lower number is rejected. Gaps are fine (a
   jump from `0` straight to `100` is accepted); only non-increasing values
   are rejected.

A rejected envelope is **never forwarded to `holo_bridge`** — the daemon
replies with `{"type":"error","text":"envelope rejected: <reason>"}`
(envelope-wrapped, echoing whatever `task_id` the rejected envelope carried)
and continues reading the next line, per the existing "malformed input is
not a transport-level error" contract (see "Error handling on malformed
input" below, which this extends rather than replaces).

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
```

| Field             | Type                                                                        | Required | Meaning |
|-------------------|------------------------------------------------------------------------------|----------|---------|
| `type`            | `"prompt"` \| `"voice_transcript"` \| `"stop"` \| `"pause"` \| `"resume"` \| `"redirect"` \| `"pin"` \| `"input_response"` | yes | Discriminant. |
| `text`            | `string`                                                                    | only for `prompt` / `voice_transcript` / `redirect` | The instruction text. Voice input is always transcribed client-side before sending — the wire format is never raw audio (see README's "Prompts" section). |
| `pin`             | `string`                                                                    | only for `pin` | The PIN the user was shown out-of-band (Mac terminal, alongside the ticket) and typed/scanned into the client. |
| `request_id`      | `string`                                                                    | only for `input_response` | Echoes the `request_id` of the `input_request` this answers. |
| `selected_option` | `string`                                                                    | only for `input_response` | The user's chosen option, expected to be one of the original `input_request.response_options`. |

- `prompt`: a typed text instruction, handed to the `holo-desktop-cli`
  bridge as-is.
- `voice_transcript`: functionally identical to `prompt` on the wire (same
  `text` field) but tagged separately so the daemon/UI can distinguish
  input modality for logging/UX (e.g. showing a mic icon in the status
  panel) without re-deriving it from context.
- `stop`: the remote **kill-switch** — cancels/interrupts whatever
  `holo-desktop-cli` is currently doing. No `text`. On the daemon this maps
  (via `control_channel::to_control_message`) to `ControlMessage::Stop` with
  no `context_id`, which `HoloControlBridge::handle_stop` handles by draining
  any queued prompts (each gets a terminal `status`/`Done{Canceled}`) and
  then engaging the CLI-level global kill switch by shelling out to
  `holo stop` (see `mac-daemon/src/holo_bridge/stop.rs`) — the same
  pause-then-cancel effect as the double-Esc / `holo stop` kill switch built
  into `holo-desktop-cli` itself. Because the wire `stop` carries no
  `context_id`, it always engages this *global* stop ("stop whatever is
  running") rather than a scoped A2A `tasks/cancel`; a future schema revision
  that threads a `context_id`/`task_id` through would let a client scope the
  stop to one specific turn.
- `pause`: parks the in-flight turn so `resume` can continue it. The Holo
  backend exposes **no pause RPC** over A2A (its own kill switch is
  pause-then-cancel — see `mac-daemon/src/holo_bridge/stop.rs`'s source
  notes), so the daemon implements pause as the only honest primitive
  available: it **cancels** the running turn (scoped A2A `tasks/cancel` when
  the turn's `contextId` has resolved, else the graceful global `holo stop`)
  while stashing the turn's original instruction text and `contextId`. The
  canceled turn still produces its normal `task_done` (`canceled`) — a
  client showing a "paused" state should expect and tolerate that terminal.
  Pausing with nothing running, or pausing twice, is a polite `status`
  reply, never an error.
- `resume`: re-dispatches the parked turn on the **same** `contextId`, so
  the backend session's history carries the task forward from where it was
  interrupted. The resumed turn runs under the `resume` envelope's own
  `task_id`. Resuming with nothing parked is a polite `status` reply.
- `redirect`: replaces whatever is running/queued with a new instruction —
  the daemon cancels the in-flight turn, drains the queue (each queued
  prompt gets its own `task_done`/`Done{Canceled}`), discards any parked
  (paused) turn, and runs `text`, reusing the canceled turn's `contextId`
  when known so the agent keeps the task history it had built up. An empty
  `text` is rejected with an `error`.
- `pin`: presents a PIN for first-connection auth (see
  `mac-daemon/PAIRING.md`'s "Auth beyond ticket possession" section). Must
  be the **first** message sent by a client whose device id is not already
  in the Mac's allowlist — the daemon does not send its normal greeting
  (`status: "control channel ready"`) until this gate passes for an
  unrecognized device, so a client should always be prepared to send `pin`
  before anything else on a fresh connection to an unfamiliar Mac. Sent
  **unwrapped**, not inside a `TaskEnvelope` — see "The one exception"
  above. Already-allowlisted devices (or a daemon run with `--no-pin-auth`)
  never need to send this message at all; if sent anyway (still unwrapped,
  since a client can't know in advance whether it'll be prompted) it is
  acknowledged and ignored (see `ControlChannel::accept`'s handling of a
  redundant `Pin`).
- `input_response`: the user's answer to a `ServerMessage`'s `input_request`
  (see below) — a **structured choice selection only**. `selected_option`
  must be one of the strings the matching `input_request.response_options`
  offered; there is deliberately no free-text field on this message, so a
  client cannot accidentally (or on purpose) put a password/PIN/MFA code
  into it. If `request_id` doesn't match a currently-outstanding
  `input_request` on this connection (already expired, already answered, or
  never sent), the daemon replies with a normal `error` event and the
  connection stays open — this is not a transport-level failure, matching
  this document's general "malformed input" philosophy below. See
  `input_request`'s own entry for the full contract, including why real
  credential/MFA/manual entry is **not** carried by this message at all.

## `ServerMessage` (Mac daemon → iOS)

The `payload` field of every outbound `TaskEnvelope` (or, for
`auth_rejected` only, the entire bare message body):

```json
{ "type": "ack" }
{ "type": "status", "text": "connected to holo-desktop-cli" }
{ "type": "task_progress", "text": "clicked Safari icon in the Dock" }
{ "type": "task_done", "status": "completed", "text": "drafted the message" }
{ "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
{ "type": "auth_rejected", "text": "incorrect PIN" }
{ "type": "input_request", "request_id": "d290f1ee-6c54-4b01-90e6-d701748f0851", "kind": "ambiguous_choice", "context": "Two calendars match 'team standup' -- which one?", "response_options": ["Work calendar", "Personal calendar"], "expires_at": 1800000120000 }
```

| Field              | Type                                                                                          | Required | Meaning |
|--------------------|-------------------------------------------------------------------------------------------------|----------|---------|
| `type`             | `"ack"` \| `"status"` \| `"error"` \| `"task_progress"` \| `"task_done"` \| `"auth_rejected"` \| `"input_request"` | yes | Discriminant. |
| `text`             | `string`                                                                                       | optional on `ack`/`status`/`error`/`task_progress`/`task_done`/`auth_rejected` | Human-readable detail. |
| `status`           | `"completed"` \| `"failed"` \| `"canceled"`                                                     | only for `task_done` | Which terminal state the task reached. |
| `request_id`       | `string`                                                                                       | only for `input_request` | Correlates this request with the eventual `ClientMessage.input_response`. |
| `kind`             | `"credential"` \| `"mfa"` \| `"ambiguous_choice"` \| `"missing_info"` \| `"sensitive_access_consent"` | only for `input_request` | Classifies *why* input is needed — see below. |
| `context`          | `string`                                                                                       | only for `input_request` | Human-readable explanation of what's needed and why. **Never contains the credential/secret value itself** — see the `input_request` section below. |
| `response_options` | `string[]`                                                                                     | only for `input_request` | The closed set of choices the user may pick from. May legitimately be `[]` for kinds with no discrete choices (`credential`, `mfa`). |
| `expires_at`       | `number`                                                                                       | only for `input_request` | Unix epoch **milliseconds** after which this request is considered expired if no matching `input_response` has arrived. Plain epoch-millis integer, not an ISO 8601 string — JSON has no native timestamp type and this crate has no `chrono`/`time` dependency. |

- `ack`: acknowledges receipt of a `ClientMessage` (e.g. the prompt was
  received and handed to the bridge). `text` is optional and, when present,
  may echo back what was acknowledged.
- `status`: a general daemon/connection status update (e.g. "connected to
  holo-desktop-cli", "broadcast started", or the initial "control channel
  ready" greeting) for the iOS status panel. Also used for the
  expiry-to-safe-pause notification described under `input_request` below.
- `task_progress`: an in-progress update from the `holo-desktop-cli` bridge
  while it's carrying out a `prompt`/`voice_transcript` (per README's "Holo
  bridge" section — "Progress/results are relayed back over the control
  channel").
- `task_done`: the terminal lifecycle signal for one task — the turn named
  by the envelope's `task_id` reached `status` (`completed` / `failed` /
  `canceled`), with optional human-readable detail in `text`. Added
  (additively) so a client's task controls have a reliable end-of-task
  signal to key off; previously a terminal was folded into free-text
  `status`/`error` lines. Note the pause interaction: pausing a task
  cancels its running turn (see `pause` above), so a `task_done` with
  `"canceled"` arrives even for a task the user considers merely paused.
- `error`: something failed (bad envelope, malformed payload, envelope
  rejected per the rules above, bridge process crashed, capture failure,
  etc.). `text` should contain enough detail to show the user, not
  necessarily a full stack trace.
- `auth_rejected`: sent instead of the normal greeting when an
  unrecognized device fails the PIN gate (wrong/missing PIN, malformed
  first message, or connection closed before presenting one) — see
  `mac-daemon/PAIRING.md`. Sent **unwrapped**, not inside a `TaskEnvelope`
  — see "The one exception" above (no `session_id` exists yet at this
  point). The connection is closed immediately after this message is sent;
  a client that receives it should return to a pairing/PIN-entry UI rather
  than treating it like a generic `error`.
- `input_request`: asks the user for structured input the agent cannot
  proceed without (Project Aro PRD row P0-14). See the dedicated section
  below — this is the most involved variant on the wire and has security
  properties the others don't.

### `input_request` / `input_response`

`input_request` (server → client) is how the daemon pauses a running turn
to ask the user something. Its `kind` is one of:

| `kind`                      | Meaning | Typical `response_options` |
|------------------------------|---------|------------------------------|
| `credential`                 | A credential (password, API key, secret token, etc.) is needed. | `[]` — no discrete choices; see "Credentials never travel on this channel" below. |
| `mfa`                        | A multi-factor authentication code/approval is needed. | `[]`, same reasoning as `credential`. |
| `ambiguous_choice`           | The agent found more than one plausible way to proceed. | The candidate options, e.g. `["Work calendar", "Personal calendar"]`. |
| `missing_info`                | The agent is missing information it cannot infer or safely guess. | Often `[]` (an open question like "which recipient email?"), but may list options when the answer is itself a closed set. |
| `sensitive_access_consent`   | The next step touches something sensitive (a payment, a destructive action, private data) and needs explicit consent first. | Typically a yes/no pair, e.g. `["Yes, proceed", "No, cancel"]`. |

**Live producer (PRD §9 sensitive-app gate).** As of this revision the
daemon actually emits `sensitive_access_consent` requests: a per-turn
watchdog polls the Mac's frontmost application while the agent acts and
classifies its bundle id against the user-editable class-5 category config
(`~/.holoiroh/sensitive_categories.toml` — see
`mac-daemon/src/sensitive_categories.rs`). An `always_ask` category match
pauses the turn (same park-the-turn mechanics as the wire `pause`) and
sends an `input_request` with `response_options`
`["Allow once", "Stop task"]`; `Allow once` resumes the turn and covers
that category for the rest of the SAME task, anything else (or expiry
after 120s) leaves the task safely stopped/paused. A `hard_block` category
cancels the turn outright with a `status` explaining why; `always_allow`
proceeds silently.

**Credentials never travel on this channel — hard requirement, not a
convention.** `input_request`'s `context`/`response_options` fields are
metadata describing *what* is needed and *why* ("Holo needs your GitHub
personal access token to push this branch", "Enter the 6-digit code from
your authenticator app") — they are never populated with the actual
credential/secret/MFA-code value, and the Rust implementation
(`ServerMessage::input_request` in `control_channel.rs`) has no parameter
through which a caller could thread one in. This directly implements the
Project Aro PRD's requirement that "credential characters are never
logged, never included in screenshots, never echoed in task events": since
`input_request` only ever announces that manual entry is needed (for the
`credential`/`mfa` kinds), the actual value is designed to flow over a
**separate `manual_input` channel** — not part of this wire schema, not
implemented by this control channel at all, and architected so it never
reaches the model/agent context (the LLM driving `holo-desktop-cli` never
sees the raw credential; only a human-operated, out-of-band path does).
Building that `manual_input` channel is tracked as its own, separate PRD
row — this document only guarantees `input_request`/`input_response` never
become an accidental backdoor for a credential to leak into a
control-channel message, a task-event log, or a screenshot.

`input_response` (client → server, see `ClientMessage` above) is the
user's answer for the `ambiguous_choice`/`missing_info`/
`sensitive_access_consent` kinds — a structured selection among
`response_options`. For `credential`/`mfa` kinds, `input_request` only
ever announces that manual entry is needed; the actual secret is provided
out of band via the separate `manual_input` channel, never as an
`input_response`.

**Expiry-to-safe-pause.** If no `input_response` arrives before
`expires_at`, the daemon does **not** treat this as a failure. It emits a
`status` message (never `error`) whose `text` says the task safely paused
and is waiting for input, e.g.:

```json
{ "type": "status", "text": "input request d290f1ee-6c54-4b01-90e6-d701748f0851 expired with no response -- task safely paused, waiting for input" }
```

The pending request is then cleared connection-side; a later
`input_response` for that same `request_id` is treated as unmatched (see
the `input_response` entry under `ClientMessage` above) rather than
resurrecting the expired request.

**At most one `input_request` outstanding per connection at a time.** This
matches the control channel's existing single-active-turn concurrency
model (one `prompt`/`voice_transcript` turn runs at a time per connection,
with others queued — see the "Holo bridge" section of `README.md`); an
`input_request` pauses that one in-flight turn, so there is no scenario
today where a second `input_request` would need to be issued before the
first is answered or expires.

## Serialization

Both `ClientMessage` and `ServerMessage` are tagged, internally-tagged enums
keyed on `type` (`#[serde(tag = "type", rename_all = "snake_case")]`),
matching the wire examples above exactly — there is no separate wrapper
object at the payload level; `type` and `text` (or `pin`) are sibling fields
of one flat JSON object per payload. `text` is `Option<String>` and is
omitted from the serialized JSON when `None`
(`#[serde(skip_serializing_if = "Option::is_none")]`), rather than emitted
as `"text": null`, so `stop` and `ack` payloads serialize as
`{"type":"stop"}` / `{"type":"ack"}` with no `text` key at all. The
`TaskEnvelope<T>` wrapper around this payload is a plain (non-tagged)
struct — `payload` is just a normal field holding the tagged enum above; see
"Envelope" for its own field table.

`input_request`'s fields (`request_id`, `kind`, `context`,
`response_options`, `expires_at`) and `input_response`'s fields
(`request_id`, `selected_option`) are **not** optional — unlike `text`,
every one of these is always present in the serialized JSON (plain
`String`/`Vec<String>`/`u64` fields, no `skip_serializing_if`), since
`input_request`/`input_response` are structured messages where a missing
field would be ambiguous rather than "absent detail." `response_options`
being empty (`[]`) is a normal, expected value for kinds with no discrete
choices (see the `input_request` section above) — it is never omitted
entirely, so a client can always index into it without a null-check.
`kind` itself serializes as a bare snake_case string (`InputRequestKind`'s
own `#[serde(rename_all = "snake_case")]`, no nested tag), sitting
directly as `input_request`'s `kind` field value.

## Error handling on malformed input

A line that fails to parse as valid JSON at the envelope level, parses as an
envelope but is missing/mistypes a required framing field, parses as a
valid envelope but its `payload` fails to parse as `ClientMessage`, or
parses fine at every level but fails one of the three rejection rules above
— **none of these are a transport-level error**. The connection and stream
stay open in every case. The receiving side logs the failure and, on the
daemon side, sends back an envelope-wrapped `{"type": "error", "text":
"..."}` describing which of these categories the failure was (the exact
text distinguishes "malformed envelope", "malformed payload", and "envelope
rejected: <expiry|duplicate|sequence reason>" — see `control_channel.rs`'s
`ProtocolHandler::accept` for the exact wording), then continues reading the
next line. Only stream/connection-level failures (EOF, reset, peer
disconnect) end the control-channel task. (The one case with no envelope to
wrap a reply in: a completely unparseable line arriving during the bare-PIN
pre-session window is handled by `authenticate`'s own gate, which rejects
the whole connection rather than replying inline — see "The one exception"
above.)

## Known gaps

- **`signature` is not cryptographically verified.** The field exists on
  the wire (present per the PRD schema, always `null`/omitted on envelopes
  this daemon constructs) but nothing in this codebase checks it against
  anything — there is no signing keypair/identity infrastructure here yet.
  The `iroh` node keypair authenticates the *transport* (who you're
  connected to), not individual envelopes; a genuine envelope-signing
  scheme (what key, over which fields, checked where) is separate,
  unbuilt work.
- **Five of the six PRD-named streams are not implemented.** See "Project
  Aro PRD context" above — this document, and this codebase, cover `control`
  only.
- **`protocol_version` mismatch is not hard-enforced.** Logged, not
  rejected, since exactly one version (`1`) exists as of this writing; a
  real enforcement policy (reject unknown versions? negotiate?) is
  unbuilt work for whenever a second version actually exists.

## Envelope versioning

`PROTOCOL_VERSION` (`control_channel::PROTOCOL_VERSION`, currently `1`) is
bumped only on a deliberate, coordinated change to this envelope shape
itself (adding/removing/retyping a top-level envelope field) — it is
independent of the crate's own `Cargo.toml` version and independent of
changes to `ClientMessage`/`ServerMessage`'s own payload shape (those are
additive-only per "Future extension" below and don't need an envelope
version bump).

## Future extension

This schema intentionally started minimal (per the task scope: `prompt` /
`voice_transcript` / `stop` and `ack` / `status` / `error` / `task_progress`)
and has grown additively since (`pin` / `auth_rejected` for pairing,
`input_request` / `input_response` for Project Aro PRD row P0-14). Fields
are additive-only going forward — new optional fields or new `type`
variants may be added, but existing field names/types and existing `type`
values must not change meaning, so that a client built against an older
revision of this document degrades gracefully (unknown `type` values
should be ignored/logged rather than treated as a hard parse error, once
the client-side implementation exists). The envelope shape itself
(`TaskEnvelope<T>`'s own fields) is versioned separately — see "Envelope
versioning" above. `input_request`/`input_response` themselves follow this
same additive policy: a client that predates this revision simply never
recognizes `input_request` and would need to fall back to
ignoring/logging it per the policy above, until it's updated.

A **separate `manual_input` channel** for real credential/secret entry is
designed but not part of this document's schema at all (see the
`input_request` / `input_response` section above for why) — it is tracked
as its own future PRD row, not an extension of this NDJSON control
channel.
