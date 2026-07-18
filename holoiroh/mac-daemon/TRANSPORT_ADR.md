# ADR-001: Remote-View media transport — H.264-over-iroh via iroh-live's MoQ

- **Status:** Accepted
- **Date:** 2026-07-18
- **PRD reference:** Project Aro PRD §7.2, Open Question **OQ-5** ("Remote View
  media transport: H.264-over-iroh primary, WebRTC fallback"); PRD row
  `holoiroh-remote-view-h264-transport`.
- **Scope:** the media (video) plane only. The control plane
  (prompts/status/`TaskEnvelope`) is a separate iroh ALPN and is out of scope
  here (see `control_channel.rs` / `PROTOCOL.md`).

## Context — what OQ-5 asks

OQ-5 names two candidate transports for streaming the Mac's screen to the iOS
client and asks which to commit to:

1. **Primary candidate — "H.264-over-iroh":** VideoToolbox-encoded H.264 frames
   streamed over a **dedicated iroh QUIC stream**, eliminating
   WebRTC/SDP/ICE/TURN entirely.
2. **Fallback — native WebRTC:** DTLS-SRTP media with offer/answer exchanged
   over an authenticated iroh signaling stream, used only if the primary path
   misses PRD §12.1 latency targets (Remote View active in <2s median).

The daemon **already** streams video by publishing an `iroh-live`
`LocalBroadcast` (`main.rs` → `capture::setup_screen_video` →
`broadcast.video().set_source(..)` → `live.publish(..)`). So the real question
this row must answer with evidence — not restate as an open choice — is:

> Does `iroh-live`'s existing MoQ-based broadcast/subscribe **already satisfy**
> "H.264-over-iroh" as OQ-5 specifies, making a separate, custom-built iroh QUIC
> video stream (or the WebRTC fallback) unnecessary?

## Decision

**Use `iroh-live`'s existing MoQ-over-iroh `LocalBroadcast`/subscribe path as
the Remote-View media transport. It *is* "H.264-over-iroh" as OQ-5 specifies. No
custom iroh QUIC video stream is needed, and WebRTC is not needed for the
primary path.** The transport is therefore effectively **already chosen and
wired** on the Mac side; the only remaining media-plane work is the iOS
**subscribe** side, which is a separate row (the iroh-FFI row,
`holoiroh-ios-iroh-ffi-integration`).

One concrete code change fell out of this investigation and has been made (see
"Encoder correction" below): the daemon now selects the **hardware VideoToolbox**
H.264 encoder instead of the software openh264 encoder it was hardcoded to.

## Evidence

All paths below are in the vendored `iroh-live` checkout pinned by
`mac-daemon/Cargo.toml` (rev `5f95758fcd1450e443a9134c9d9342bcc3957b85`), read
directly from
`~/.cargo/git/checkouts/iroh-live-631d06084fd6c270/5f95758/`. This is the actual
compiled source, not documentation or guesswork.

### 1. iroh-live's transport IS H.264 over iroh QUIC streams — verbatim from n0

`iroh-live`'s own README self-description
(`iroh-live/README.md`):

> "Real-time audio and video over **iroh (QUIC)** … The transport layer uses
> **Media over QUIC (MoQ)**, where **each video rendition and audio track
> travels as an independent QUIC stream**, so a dropped video packet never
> blocks audio delivery."

`iroh-moq/src/lib.rs` (module doc, line 1): *"MoQ transport layer over iroh.
Provides `Moq`/`MoqSession` for publish/subscribe operations **over QUIC
connections**."* Its ALPN is `moq-lite-04` (`iroh-moq/src/lib.rs:35`); a session
is established via `endpoint.connect(addr, ALPN)` →
`web_transport_iroh::Session` over an iroh QUIC `Connection`
(`iroh-moq/src/lib.rs:283-285`).

**This is exactly the "dedicated iroh QUIC stream" OQ-5 describes for the
primary candidate.** MoQ already provides per-rendition independent QUIC
streams on the iroh endpoint. A hand-rolled custom iroh QUIC video framing would
re-implement, worse and unmaintained, what MoQ already gives us.

### 2. The codec on the wire is H.264 — and hardware VideoToolbox is available

`VideoCodec` (the enum `main.rs` passes to `set_source`) is
`rusty_codecs::codec::VideoCodec`, re-exported as `iroh_live::media::codec`
(`moq-media/src/lib.rs:39` → `pub use rusty_codecs::{codec, ...}`;
`iroh-live/src/lib.rs:21` → `pub use moq_media as media`). Its variants
(`rusty-codecs/src/codec.rs:102-125`):

- `H264` — *"Software H.264 via openh264."* Encoder `const ID = "h264-openh264"`
  (`rusty-codecs/src/codec/h264/encoder.rs:180`).
- `VtbH264` — *"**Hardware H.264 via macOS VideoToolbox.**"* Encoder
  `const ID = "h264-vtb"` (`rusty-codecs/src/codec/vtb/encoder.rs:205`), which
  drives `kCMVideoCodecType_H264 = 'avc1'` (0x61766331) with
  `kVTProfileLevel_H264_Baseline_AutoLevel`
  (`rusty-codecs/src/codec/vtb/encoder.rs:111,154`).

Both produce **standard H.264/AVC on the wire**; the difference is only the
encoder implementation (hardware VideoToolbox vs software openh264). The catalog
codec identity is H.264 either way, so the iOS decode path
(`AVSampleBufferDisplayLayer`, `ios/.../Video/VideoRenderView.swift`) is
unaffected by the choice.

`VideoCodec::best_available()` prefers hardware over software and returns
`Some(VtbH264)` on macOS when the `videotoolbox` feature is compiled in
(`rusty-codecs/src/codec.rs:154-176`).

### 3. The `videotoolbox` feature IS compiled into this daemon build

`iroh-live`'s **default** feature set includes `videotoolbox`
(`iroh-live/Cargo.toml:62`:
`default = ["h264", "opus", "capture", "wgpu", "vaapi", "videotoolbox", ...]`).
`mac-daemon/Cargo.toml:28` depends on `iroh-live` with default features (no
`default-features = false`), so it is on.

Witnessed against the *resolved* build graph (not just the manifest) via
`cargo tree -p holoiroh-daemon -e features`:

```
iroh-live feature "default"
  └── iroh-live feature "videotoolbox"
        └── moq-media feature "videotoolbox"
              └── rusty-codecs feature "videotoolbox"  (also pulls apple-gpu)
```

So `#[cfg(feature = "videotoolbox")]` is active in this build:
`VideoCodec::VtbH264` exists and `moq_media::publish::add()` maps it to
`codec::VtbEncoder` (`moq-media/src/publish.rs:998-999`).

### 4. The daemon already publishes this transport (Mac side is wired)

`main.rs` brings up `Live::from_env().await?.spawn()`, mounts MoQ + control ALPN
on one shared `Router` (`live.register_protocols(router_builder)`), attaches the
ScreenCaptureKit source to a `LocalBroadcast`, and calls
`live.publish(BROADCAST_NAME, &broadcast)` — which is
`moq.publish(name, broadcast.producer())` (`iroh-live/src/live.rs:204-206`),
announcing the broadcast over MoQ to every connected/future peer. The subscriber
side of the same library (`Live::subscribe` → `Subscription::media_with_decoders`,
`iroh-live/src/live.rs:229-279`, `iroh-live/src/subscription.rs:62-81`) is the
symmetric decode API the iOS client will call through the FFI bridge.

## Why not the alternatives

**Custom low-level iroh QUIC video stream — rejected.** OQ-5 floats this only
"if the primary path misses the PRD's specific latency targets." There is no
evidence it does; MoQ-over-iroh is *already* per-rendition independent QUIC
streams (Evidence §1), i.e. the exact primitive a custom stream would provide.
Building bespoke framing/congestion/keyframe-request logic on raw
`iroh::Connection` streams would duplicate `iroh-moq` + `moq-media` (encode,
catalog, rendition selection, `NetworkSignals`-driven adaptive bitrate —
`subscription.rs:54-56`) with an unmaintained fork. It only becomes justified if
a *measured* latency miss is traced specifically to MoQ overhead — which
requires the live daemon running past the macOS TCC gate (blocked; see below).
Deferring it is correct: it is a contingency, not the plan.

**Native WebRTC (DTLS-SRTP) fallback — not needed for the primary path, kept
only as a contingency.** WebRTC would re-introduce exactly what OQ-5's primary
candidate exists to eliminate: SDP/ICE/TURN and a signaling server. iroh already
solves NAT traversal (hole-punch + relay fallback) and ticket-based dialing
without any of that. WebRTC stays a documented fallback for a *measured*
primary-path failure, not something to build now.

## Encoder correction made in this pass (real code change)

**Finding:** despite VideoToolbox being compiled in (Evidence §3), `main.rs`
hardcoded `VideoCodec::H264` — the **software openh264** encoder
(`moq-media/src/publish.rs:995`: `VideoCodec::H264 => codec::H264Encoder`). So
the daemon was *not* actually using VideoToolbox, contrary to the "H.264/
VideoToolbox on macOS" phrasing in the README and OQ-5's "VideoToolbox-encoded
frames" intent. Software H.264 at 720p means higher CPU and higher
encode latency — directly counter to the PRD §12.1 <2s / low-latency goal.

**Fix:** `main.rs` now selects the encoder via
`VideoCodec::best_available().unwrap_or(VideoCodec::H264)`, which returns
`VtbH264` (hardware VideoToolbox) on this macOS build and falls back to software
openh264 only if no hardware encoder is available. This matches `iroh-live`'s
own reference CLI, which defaults the codec via
`VideoCodec::parse_or_best(None)` → `best_available()`
(`iroh-live-cli/src/args.rs:129-130`, `rusty-codecs/src/codec.rs:253-260`). The
wire codec stays H.264/AVC either branch, so the iOS decoder is unchanged; the
fallback is a graceful CPU-cost degradation, never a format break.

**Witness:** `cargo build -p holoiroh-daemon` is clean after the change
(`Finished dev [unoptimized + debuginfo] in 5.18s`), and the feature-graph proof
in Evidence §3 shows `best_available()` deterministically resolves to `VtbH264`
in this build. A *runtime* confirmation of the emitted encoder id
(`h264-vtb`) additionally requires the daemon to run past the macOS Screen
Recording/Accessibility TCC preflight, which is the standing external blocker
tracked by `holoiroh-user-action-grant-tcc-and-run-daemon` and the
`holoiroh-e2e-witness-blocked-by-tcc-permissions` mutable (Apple provides no CLI
to grant TCC; interactive System Settings only).

## Consequences

- The Mac-side media transport for Remote View is **decided and wired**:
  `iroh-live` MoQ-over-iroh, hardware VideoToolbox H.264. No new transport crate,
  no WebRTC stack, no custom QUIC framing.
- `holoiroh-remote-view-h264-transport` collapses from "evaluate + build a
  transport" to "the transport is `iroh-live`'s existing path; confirm latency
  once the daemon can run." Its remaining acceptance (Remote View <2s median;
  snapshot fallback on stream loss; input-never-before-active ordering) is
  **downstream of the iOS subscribe side and the TCC-gated live run**, not of a
  transport choice.
- The one media-plane piece still genuinely unbuilt is the **iOS subscribe**
  path — `Live::subscribe`/`RemoteBroadcast::media_with_decoders` reached from
  Swift through the Rust FFI bridge (`ios-bridge/`), tracked as
  `holoiroh-ios-iroh-ffi-integration`. The renderer that consumes those decoded
  frames already exists (`ios/.../Video/VideoRenderView.swift`).
- WebRTC and custom-QUIC remain **documented contingencies** for a *measured*
  latency miss, not planned work.
