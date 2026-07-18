# HoloIroh control-channel protocol

Defines the JSON message schema exchanged between the iOS app (`ios/`) and
the Mac daemon (`mac-daemon/`) over the **control channel**: a second,
bidirectional logical stream running alongside the `iroh-live` media
broadcast, on the same `iroh` `Endpoint` (see `README.md`'s "Control
channel" section and "Why iroh / iroh-live specifically" for the
architecture rationale).

This document is the source of truth for the wire schema. The Rust types in
`mac-daemon/src/control_channel.rs` (`ClientMessage`, `ServerMessage`)
implement it via `serde`; any change here must be mirrored there, and
eventually in the Swift client.

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

## `ClientMessage` (iOS → Mac daemon)

```json
{ "type": "prompt", "text": "open safari and check my calendar" }
{ "type": "voice_transcript", "text": "what's on my screen right now" }
{ "type": "stop" }
{ "type": "pin", "pin": "123456" }
{ "type": "input_response", "request_id": "d290f1ee-6c54-4b01-90e6-d701748f0851", "selected_option": "Work calendar" }
```

| Field             | Type                                                                        | Required | Meaning |
|-------------------|------------------------------------------------------------------------------|----------|---------|
| `type`            | `"prompt"` \| `"voice_transcript"` \| `"stop"` \| `"pin"` \| `"input_response"` | yes | Discriminant. |
| `text`            | `string`                                                                    | only for `prompt` / `voice_transcript` | The instruction text. Voice input is always transcribed client-side before sending — the wire format is never raw audio (see README's "Prompts" section). |
| `pin`             | `string`                                                                    | only for `pin` | The PIN the user was shown out-of-band (Mac terminal, alongside the ticket) and typed/scanned into the client. |
| `request_id`      | `string`                                                                    | only for `input_response` | Echoes the `request_id` of the `input_request` this answers. |
| `selected_option` | `string`                                                                    | only for `input_response` | The user's chosen option, expected to be one of the original `input_request.response_options`. |

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
  before anything else on a fresh connection to an unfamiliar Mac. Already-
  allowlisted devices (or a daemon run with `--no-pin-auth`) never need to
  send this message at all; if sent anyway it is acknowledged and ignored
  (see `ControlChannel::accept`'s handling of a redundant `Pin`).
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

```json
{ "type": "ack" }
{ "type": "status", "text": "connected to holo-desktop-cli" }
{ "type": "task_progress", "text": "clicked Safari icon in the Dock" }
{ "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
{ "type": "auth_rejected", "text": "incorrect PIN" }
{ "type": "input_request", "request_id": "d290f1ee-6c54-4b01-90e6-d701748f0851", "kind": "ambiguous_choice", "context": "Two calendars match 'team standup' -- which one?", "response_options": ["Work calendar", "Personal calendar"], "expires_at": 1800000120000 }
```

| Field              | Type                                                                                          | Required | Meaning |
|--------------------|-------------------------------------------------------------------------------------------------|----------|---------|
| `type`             | `"ack"` \| `"status"` \| `"error"` \| `"task_progress"` \| `"auth_rejected"` \| `"input_request"` | yes | Discriminant. |
| `text`             | `string`                                                                                       | optional on `ack`/`status`/`error`/`task_progress`/`auth_rejected` | Human-readable detail. |
| `request_id`       | `string`                                                                                       | only for `input_request` | Correlates this request with the eventual `ClientMessage.input_response`. |
| `kind`             | `"credential"` \| `"mfa"` \| `"ambiguous_choice"` \| `"missing_info"` \| `"sensitive_access_consent"` | only for `input_request` | Classifies *why* input is needed — see below. |
| `context`          | `string`                                                                                       | only for `input_request` | Human-readable explanation of what's needed and why. **Never contains the credential/secret value itself** — see the `input_request` section below. |
| `response_options` | `string[]`                                                                                     | only for `input_request` | The closed set of choices the user may pick from. May legitimately be `[]` for kinds with no discrete choices (`credential`, `mfa`). |
| `expires_at`       | `number`                                                                                       | only for `input_request` | Unix epoch **milliseconds** after which this request is considered expired if no matching `input_response` has arrived. Plain epoch-millis integer, not an ISO 8601 string — JSON has no native timestamp type and this crate has no `chrono`/`time` dependency. |

- `ack`: acknowledges receipt of a `ClientMessage` (e.g. the prompt was
  received and handed to the bridge). `text` is optional and, when present,
  may echo back what was acknowledged.
- `status`: a general daemon/connection status update (e.g. "connected to
  holo-desktop-cli", "broadcast started") for the iOS status panel. Also
  used for the expiry-to-safe-pause notification described under
  `input_request` below.
- `task_progress`: an in-progress update from the `holo-desktop-cli` bridge
  while it's carrying out a `prompt`/`voice_transcript` (per README's "Holo
  bridge" section — "Progress/results are relayed back over the control
  channel").
- `error`: something failed (bad JSON from the client, bridge process
  crashed, capture failure, etc.). `text` should contain enough detail to
  show the user, not necessarily a full stack trace.
- `auth_rejected`: sent instead of the normal greeting when an
  unrecognized device fails the PIN gate (wrong/missing PIN, malformed
  first message, or connection closed before presenting one) — see
  `mac-daemon/PAIRING.md`. The connection is closed immediately after this
  message is sent; a client that receives it should return to a
  pairing/PIN-entry UI rather than treating it like a generic `error`.
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

Both types are tagged, internally-tagged enums keyed on `type`
(`#[serde(tag = "type", rename_all = "snake_case")]`), matching the wire
examples above exactly — there is no separate wrapper object; `type` and
`text` are sibling fields of one flat JSON object per message. `text` is
`Option<String>` and is omitted from the serialized JSON when `None`
(`#[serde(skip_serializing_if = "Option::is_none")]`), rather than emitted
as `"text": null`, so `stop` and `ack` messages serialize as `{"type":"stop"}`
/ `{"type":"ack"}` with no `text` key at all — this is what the wire
examples above show and is exactly what the JSON schema in the task
description specifies (`text?: string`, i.e. an optional/absent field, not
a nullable one).

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

A line that fails to parse as valid JSON, or parses but doesn't match
either schema, is **not** a transport-level error — the connection and
stream stay open. The receiving side logs the parse failure and, on the
daemon side, sends back `{"type": "error", "text": "..."}` describing the
problem, then continues reading the next line. Only stream/connection-level
failures (EOF, reset, peer disconnect) end the control-channel task.

## Future extension

This schema intentionally started minimal (`prompt` / `voice_transcript` /
`stop` and `ack` / `status` / `error` / `task_progress`) and has grown
additively since (`pin` / `auth_rejected` for pairing, `input_request` /
`input_response` for Project Aro PRD row P0-14). Fields are additive-only
going forward — new optional fields or new `type` variants may be added,
but existing field names/types and existing `type` values must not change
meaning, so that a client built against an older revision of this document
degrades gracefully (unknown `type` values should be ignored/logged rather
than treated as a hard parse error, once the client-side implementation
exists). `input_request`/`input_response` themselves follow this same
policy: a client that predates this revision simply never recognizes
`input_request` and would need to fall back to ignoring/logging it per the
policy above, until it's updated.

A **separate `manual_input` channel** for real credential/secret entry is
designed but not part of this document's schema at all (see the
`input_request` / `input_response` section above for why) — it is tracked
as its own future PRD row, not an extension of this NDJSON control
channel.
