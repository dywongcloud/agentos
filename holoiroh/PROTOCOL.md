# HoloIroh control-channel protocol

Defines the JSON message schema exchanged between the iOS app (`ios/`) and
the Mac daemon (`mac-daemon/`) over the **control channel**: a second,
bidirectional logical stream running alongside the `iroh-live` media
broadcast, on the same `iroh` `Endpoint` (see `README.md`'s "Control
channel" section and "Why iroh / iroh-live specifically" for the
architecture rationale).

This document is the source of truth for the wire schema. The Rust types in
`mac-daemon/src/control_channel.rs` (`ClientMessage`, `ServerMessage`,
`TaskEnvelope<T>`) implement it via `serde`; any change here must be
mirrored there, and eventually in the Swift client.

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
{ "type": "pin", "pin": "123456" }
```

| Field  | Type                                             | Required | Meaning |
|--------|-----------------------------------------------------|----------|---------|
| `type` | `"prompt"` \| `"voice_transcript"` \| `"stop"` \| `"pin"` | yes | Discriminant. |
| `text` | `string`                                           | only for `prompt` / `voice_transcript` | The instruction text. Voice input is always transcribed client-side before sending — the wire format is never raw audio (see README's "Prompts" section). |
| `pin`  | `string`                                           | only for `pin` | The PIN the user was shown out-of-band (Mac terminal, alongside the ticket) and typed/scanned into the client. |

- `prompt`: a typed text instruction, handed to the `holo-desktop-cli`
  bridge as-is.
- `voice_transcript`: functionally identical to `prompt` on the wire (same
  `text` field) but tagged separately so the daemon/UI can distinguish
  input modality for logging/UX (e.g. showing a mic icon in the status
  panel) without re-deriving it from context.
- `stop`: cancels/interrupts whatever `holo-desktop-cli` is currently
  doing. No `text`.
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

## `ServerMessage` (Mac daemon → iOS)

The `payload` field of every outbound `TaskEnvelope` (or, for
`auth_rejected` only, the entire bare message body):

```json
{ "type": "ack" }
{ "type": "status", "text": "connected to holo-desktop-cli" }
{ "type": "task_progress", "text": "clicked Safari icon in the Dock" }
{ "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
{ "type": "auth_rejected", "text": "incorrect PIN" }
```

| Field  | Type                                                       | Required | Meaning |
|--------|-----------------------------------------------------------|----------|---------|
| `type` | `"ack"` \| `"status"` \| `"error"` \| `"task_progress"` \| `"auth_rejected"` | yes | Discriminant. |
| `text` | `string`                                                     | optional on every variant | Human-readable detail. |

- `ack`: acknowledges receipt of a `ClientMessage` (e.g. the prompt was
  received and handed to the bridge). `text` is optional and, when present,
  may echo back what was acknowledged.
- `status`: a general daemon/connection status update (e.g. "connected to
  holo-desktop-cli", "broadcast started", or the initial "control channel
  ready" greeting) for the iOS status panel.
- `task_progress`: an in-progress update from the `holo-desktop-cli` bridge
  while it's carrying out a `prompt`/`voice_transcript` (per README's "Holo
  bridge" section — "Progress/results are relayed back over the control
  channel").
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

The `ClientMessage`/`ServerMessage` payload schema intentionally starts
minimal (per the task scope: `prompt` / `voice_transcript` / `stop` and
`ack` / `status` / `error` / `task_progress` / `auth_rejected`). Fields are
additive-only going forward — new optional fields or new `type` variants
may be added, but existing field names/types and existing `type` values
must not change meaning, so that a client built against an older revision
of this document degrades gracefully (unknown `type` values should be
ignored/logged rather than treated as a hard parse error, once the
client-side implementation exists). The envelope shape itself
(`TaskEnvelope<T>`'s own fields) is versioned separately — see "Envelope
versioning" above.
