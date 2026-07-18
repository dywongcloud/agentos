# HoloIroh control-channel protocol

Defines the JSON message schema exchanged between the iOS app (`ios/`) and
the Mac daemon (`mac-daemon/`) over the **control channel**: a second,
bidirectional logical stream running alongside the `iroh-live` media
broadcast, on the same `iroh` `Endpoint` (see `README.md`'s "Control
channel" section and "Why iroh / iroh-live specifically" for the
architecture rationale).

This document is the source of truth for the wire schema. The Rust types in
`mac-daemon/src/control.rs` (`ClientMessage`, `ServerMessage`) implement it
via `serde`; any change here must be mirrored there, and eventually in the
Swift client.

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
  before anything else on a fresh connection to an unfamiliar Mac. Already-
  allowlisted devices (or a daemon run with `--no-pin-auth`) never need to
  send this message at all; if sent anyway it is acknowledged and ignored
  (see `ControlChannel::accept`'s handling of a redundant `Pin`).

## `ServerMessage` (Mac daemon → iOS)

```json
{ "type": "ack" }
{ "type": "status", "text": "connected to holo-desktop-cli" }
{ "type": "task_progress", "text": "clicked Safari icon in the Dock" }
{ "type": "error", "text": "holo-desktop-cli exited unexpectedly (code 1)" }
{ "type": "auth_rejected", "text": "incorrect PIN" }
```

| Field  | Type                                                       | Required | Meaning |
|--------|---------------------------------------------------------------|----------|---------|
| `type` | `"ack"` \| `"status"` \| `"error"` \| `"task_progress"` \| `"auth_rejected"` | yes | Discriminant. |
| `text` | `string`                                                     | optional on every variant | Human-readable detail. |

- `ack`: acknowledges receipt of a `ClientMessage` (e.g. the prompt was
  received and handed to the bridge). `text` is optional and, when present,
  may echo back what was acknowledged.
- `status`: a general daemon/connection status update (e.g. "connected to
  holo-desktop-cli", "broadcast started") for the iOS status panel.
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

## Error handling on malformed input

A line that fails to parse as valid JSON, or parses but doesn't match
either schema, is **not** a transport-level error — the connection and
stream stay open. The receiving side logs the parse failure and, on the
daemon side, sends back `{"type": "error", "text": "..."}` describing the
problem, then continues reading the next line. Only stream/connection-level
failures (EOF, reset, peer disconnect) end the control-channel task.

## Future extension

This schema intentionally starts minimal (per the task scope: `prompt` /
`voice_transcript` / `stop` and `ack` / `status` / `error` /
`task_progress`). Fields are additive-only going forward — new optional
fields or new `type` variants may be added, but existing field names/types
and existing `type` values must not change meaning, so that a client built
against an older revision of this document degrades gracefully (unknown
`type` values should be ignored/logged rather than treated as a hard
parse error, once the client-side implementation exists).
