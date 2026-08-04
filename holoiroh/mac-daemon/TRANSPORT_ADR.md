# ADR-001: Remote-View media transport — H.264 over iroh-live Media over QUIC

- **Status:** Accepted
- **Date:** 2026-07-18
- **PRD reference:** Project Aro product requirements document (PRD) §7.2, Open Question 5 (OQ-5).
- **PRD row:** `holoiroh-remote-view-h264-transport`.
- **Scope:** This decision covers only the video media plane.
- **Excluded scope:** The control plane uses a separate iroh application-layer protocol negotiation (ALPN) identifier.
- **Control references:** See `control_channel.rs` and `PROTOCOL.md` for prompts, status, and `TaskEnvelope`.

## Context

OQ-5 identifies two media transport candidates:

1. **Primary:** Send VideoToolbox-encoded H.264 frames over a dedicated iroh QUIC stream.
   QUIC was originally named Quick UDP Internet Connections.
2. **Fallback:** Send native WebRTC media with Datagram Transport Layer Security–Secure Real-time Transport Protocol (DTLS-SRTP).

The fallback uses offer and answer exchange over an authenticated iroh signaling stream.
It applies only if the primary path misses the PRD §12.1 latency target.
That target requires Remote View to become active in less than 2 seconds median.

The daemon publishes an `iroh-live` `LocalBroadcast` through this path:

`main.rs` → `capture::setup_screen_video` → `broadcast.video().set_source(..)` → `live.publish(..)`

The decision therefore concerns the existing Media over QUIC (MoQ) path.
The evidence must show whether this path satisfies OQ-5.

## Decision

Use the existing `iroh-live` MoQ-over-iroh `LocalBroadcast` and subscribe path.
This path implements H.264 over iroh as OQ-5 specifies.
Do not add custom QUIC video framing.
Do not add WebRTC for the primary path.

The daemon publishes the media stream.
The app subscribes to the media stream through the implemented Rust bridge.
The bridge is `holoiroh-ios-bridge`.

The daemon selects `VideoCodec::best_available().unwrap_or(VideoCodec::H264)`.
Production permits a software openh264 fallback.
Therefore, do not infer runtime hardware enforcement from the `h264-vtb` selection.

Real VideoToolbox encode and decode probes confirm hardware H.264 on the target Mac.
No Transparency, Consent, and Control (TCC) blocker remains for that hardware claim.

## Evidence

The current media dependencies resolve from `../holoiroh-vendor/iroh-live-patched`.
The older Cargo Git checkout is not the current compiled source.
Its pinned revision was `5f95758fcd1450e443a9134c9d9342bcc3957b85`.
The old checkout path was `~/.cargo/git/checkouts/iroh-live-631d06084fd6c270/5f95758/`.

### 1. `iroh-live` sends media over iroh QUIC streams

The `iroh-live/README.md` description states:

> "Real-time audio and video over **iroh (QUIC)** … The transport layer uses
> **Media over QUIC (MoQ)**, where **each video rendition and audio track
> travels as an independent QUIC stream**, so a dropped video packet never
> blocks audio delivery."

`iroh-moq/src/lib.rs` describes an MoQ transport over iroh.
It provides `Moq` and `MoqSession` publish and subscribe operations over QUIC connections.
Its ALPN identifier is `moq-lite-04` at `iroh-moq/src/lib.rs:35`.

The library establishes a session through `endpoint.connect(addr, ALPN)`.
It then creates `web_transport_iroh::Session` over an iroh QUIC `Connection`.
See `iroh-moq/src/lib.rs:283-285`.

This design supplies the independent QUIC streams that OQ-5 requires.
Custom framing would duplicate `iroh-moq` and `moq-media`.

### 2. The wire codec is H.264

`main.rs` passes `rusty_codecs::codec::VideoCodec` to `set_source`.
`iroh_live::media::codec` re-exports this type.
See `moq-media/src/lib.rs:39` and `iroh-live/src/lib.rs:21`.

`rusty-codecs/src/codec.rs:102-125` defines these relevant variants:

- `H264` uses software openh264.
  Its encoder identifier is `h264-openh264` at `rusty-codecs/src/codec/h264/encoder.rs:180`.
- `VtbH264` uses macOS VideoToolbox H.264.
  Its encoder identifier is `h264-vtb` at `rusty-codecs/src/codec/vtb/encoder.rs:205`.
  It uses `kCMVideoCodecType_H264 = 'avc1'` (`0x61766331`).
  It uses `kVTProfileLevel_H264_Baseline_AutoLevel`.
  See `rusty-codecs/src/codec/vtb/encoder.rs:111,154`.

Both variants produce standard H.264/Advanced Video Coding (AVC) data.
The encoder implementation does not change the catalog codec identity.

`VideoCodec::best_available()` prefers `VtbH264` when the `videotoolbox` feature is available on macOS.
See `rusty-codecs/src/codec.rs:154-176`.
Production can still use software fallback at runtime.

### 3. Both production feature graphs include `videotoolbox`

`iroh-live` includes `videotoolbox` in its default features.
See `iroh-live/Cargo.toml:62`:

`default = ["h264", "opus", "capture", "wgpu", "vaapi", "videotoolbox", ...]`

`mac-daemon/Cargo.toml:28` does not set `default-features = false`.
The resolved daemon and bridge feature graphs include `h264` and `videotoolbox`.
They do not include `av1`.

The daemon graph included this exact result from `cargo tree -p holoiroh-daemon -e features`:

```
iroh-live feature "default"
  └── iroh-live feature "videotoolbox"
        └── moq-media feature "videotoolbox"
              └── rusty-codecs feature "videotoolbox"  (also pulls apple-gpu)
```

This build includes `VideoCodec::VtbH264`.
`moq_media::publish::add()` maps that variant to `codec::VtbEncoder`.
See `moq-media/src/publish.rs:998-999`.

### 4. The daemon publishes the media stream

`main.rs` uses these ordered steps:

1. Start `Live::from_env().await?.spawn()`.
2. Mount MoQ and the control ALPN identifier on one shared `Router`.
   The call is `live.register_protocols(router_builder)`.
3. Attach the ScreenCaptureKit source to a `LocalBroadcast`.
4. Call `live.publish(BROADCAST_NAME, &broadcast)`.

The last call uses `moq.publish(name, broadcast.producer())`.
See `iroh-live/src/live.rs:204-206`.
It announces the broadcast to connected and future peers.

### 5. The app subscribes through the bridge

The iOS subscribe path and Rust bridge are implemented.
The bridge calls the library subscribe and decode APIs.
These APIs include `Live::subscribe` and `Subscription::media_with_decoders`.
See `iroh-live/src/live.rs:229-279` and `iroh-live/src/subscription.rs:62-81`.

`ios-bridge/src/lib.rs` requests decoded frames from the dynamic decoder.
The patched library selects VideoToolbox H.264 on iOS.
The bridge copies decoded blue-green-red-alpha (BGRA) frames to Swift.
`ios/.../Video/VideoRenderView.swift` receives those decoded frames.

## Rejected alternatives

### Custom QUIC video framing

Reject custom QUIC video framing unless a measurement identifies MoQ overhead as the latency cause.
No current evidence identifies such a cause.

MoQ already supplies independent QUIC streams for each rendition.
`iroh-moq` and `moq-media` also supply encoding, catalogs, and rendition selection.
They supply `NetworkSignals`-driven adaptive bitrate at `subscription.rs:54-56`.
Custom framing would duplicate those functions.

### Native WebRTC fallback

Keep WebRTC only as a contingency for a measured primary-path failure.
WebRTC would add Session Description Protocol (SDP).
It would add Interactive Connectivity Establishment (ICE).
It would also add Traversal Using Relays around NAT (TURN) and a signaling server.

iroh already supplies Network Address Translation (NAT) traversal.
It uses hole punching and relay fallback.
It also supplies ticket-based dialing.

## Encoder correction

`main.rs` previously selected `VideoCodec::H264`.
That variant selects software openh264 at `moq-media/src/publish.rs:995`.
The mapping is `VideoCodec::H264 => codec::H264Encoder`.

`main.rs` now selects `VideoCodec::best_available().unwrap_or(VideoCodec::H264)`.
On this build, the function selects the `VtbH264` variant first.
Production still permits software fallback when hardware encoding is unavailable.
This behavior matches the `iroh-live` command-line interface (CLI).
The CLI uses `VideoCodec::parse_or_best(None)` and then `best_available()`.
See `iroh-live-cli/src/args.rs:129-130` and `rusty-codecs/src/codec.rs:253-260`.

Both branches keep H.264/AVC on the wire.
Therefore, the decoder format does not change.

The build witness was `cargo build -p holoiroh-daemon`.
Its exact result was `Finished dev [unoptimized + debuginfo] in 5.18s`.

The codec probe below required a hardware H.264 session.
That real encode probe succeeded on the target Mac.
`VTIsHardwareDecodeSupported` also confirmed hardware H.264 decode.
These results replace the former feature-graph-only hardware claim.
They also remove the former TCC blocker for that claim.

## Consequences

- The daemon and app use the implemented `iroh-live` MoQ-over-iroh media stream.
- The transport requires no new transport crate, WebRTC stack, or custom QUIC framing.
- Production prefers VideoToolbox H.264 but permits software fallback.
- The hardware-required probe confirms H.264 hardware encoding on the target Mac.
- The decode capability probe confirms H.264 hardware decoding on the target Mac.
- Remote View still targets less than 2 seconds median activation time.
- Stream-loss snapshot fallback and input ordering remain separate acceptance criteria.
- WebRTC and custom QUIC framing remain contingencies for a measured latency failure.

## Codec evaluation - 2026-08-02

### Decision

Keep hardware H.264. Do not add a production codec switch.

The target Mac has hardware H.264 and High Efficiency Video Coding (HEVC) encoders.
It has no Alliance for Open Media Video 1 (AV1) encoder.
HEVC Main reduced the synthetic desktop probe size by 25.065 percent.
This result is not sufficient to ship HEVC for these reasons:

1. VideoToolbox did not expose an HEVC screen-content-coding property.
2. The installed Rust media stack supports H.264 end to end. It does not
   support HEVC end to end.
3. The iPad Simulator is not proof of physical iPhone or iPad hardware decode.
4. No physical-device HEVC decode and render result is available.

The HEVC result measures HEVC Main.
It does not measure HEVC screen content coding (SCC).
Do not claim an SCC benefit from this result.

### Test target

- Computer: MacBook Pro `Mac15,6`
- Processor: Apple M3 Pro
- Memory: 36 GB
- Architecture: arm64
- macOS: 26.3.1 (a), build `25D771280a`
- Xcode: 26.4.1, build `17E202`
- macOS SDK: 26.4
- iPhone Simulator SDK: 26.4
- Swift: 6.3.1
- Simulator: iPad Pro 11-inch (M5), `iPad17,2`
- Simulator runtime: iPadOS 26.4.1

The hardware and toolchain command was:

```sh
uname -m
sw_vers
system_profiler SPHardwareDataType
xcodebuild -version
xcrun --sdk macosx --show-sdk-version
xcrun --sdk iphonesimulator --show-sdk-version
xcrun swiftc --version
```

### Mac VideoToolbox capability witness

The installed software development kit (SDK) is version 26.4.
Its headers contain these public application programming interfaces (APIs):

- `VTCopyVideoEncoderList`
- `kVTVideoEncoderList_IsHardwareAccelerated`
- `kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder`
- `kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder`
- `VTSessionCopySupportedPropertyDictionary`
- `VTIsHardwareDecodeSupported`

The headers contain HEVC Main, Main10, and Main42210 profiles.
They expose no screen-content, palette, intra-block-copy (IBC), or SCC property.
Header inspection used this command:

```sh
SDK="$(xcrun --sdk macosx --show-sdk-path)"
rg -n 'VTCopyVideoEncoderList|VTIsHardwareDecodeSupported|HardwareAcceleratedVideoEncoder|SupportedPropertyDictionary' \
  "$SDK/System/Library/Frameworks/VideoToolbox.framework/Headers"
rg -ni 'ScreenContent|screen content|Palette|IntraBlockCopy|intra block copy|HEVCSCC|HEVC_SCC|IBC' \
  "$SDK/System/Library/Frameworks/VideoToolbox.framework/Headers" \
  "$SDK/System/Library/Frameworks/CoreMedia.framework/Headers"
```

The capability probe used the hardware-required session option.
It also read `UsingHardwareAcceleratedVideoEncoder`.
The command was:

```sh
xcrun swiftc -target arm64-apple-macosx26.0 \
  -framework VideoToolbox -framework CoreMedia \
  ios/Probes/VideoToolboxCapabilityProbe.swift \
  -o /tmp/holoiroh-vt-capability-probe
/tmp/holoiroh-vt-capability-probe
```

The exact codec output was:

```text
encoder_list codec=H.264 fourcc=avc1 entries=2
  encoder id=com.apple.videotoolbox.videoencoder.ave.avc name=Apple H.264 (HW) hardware=true
  encoder id=com.apple.videotoolbox.videoencoder.h264 name=Apple H.264 (SW) hardware=missing
encoder_list codec=HEVC fourcc=hvc1 entries=2
  encoder id=com.apple.videotoolbox.videoencoder.ave.hevc name=Apple HEVC (HW) hardware=true
  encoder id=com.apple.videotoolbox.videoencoder.hevc.vcp name=Apple HEVC (SW) hardware=missing
encoder_list codec=AV1 fourcc=av01 entries=0
hardware_decode codec=H.264 supported=true
hardware_encode codec=H.264 create_status=0 created=true hardware_query_status=0 hardware=true
hardware_decode codec=HEVC supported=true
hardware_encode codec=HEVC create_status=0 created=true hardware_query_status=0 hardware=true
hardware_decode codec=AV1 supported=true
hardware_encode codec=AV1 create_status=-12908 created=false
```

Status `-12908` is `kVTCouldNotFindVideoEncoderErr`.
This result witnesses the unavailable AV1 hardware encoder on this Mac.
The encoder list also contains no software AV1 encoder.

The HEVC session reported 159 supported properties.
No property name matched screen content, palette, intra-block copy, IBC, or SCC.
The probe also tried eight explicit property names.
Each name was absent from the supported property dictionary.
Each set operation returned `-12900`.
This status is `kVTPropertyNotSupportedErr`:

```text
supported_properties codec=HEVC status=0 total=159 screen_content_matches=[]
hevc_scc_property key=EnableHEVCScreenContentCoding advertised=false set_true_status=-12900
hevc_scc_property key=HEVCScreenContentCoding advertised=false set_true_status=-12900
hevc_scc_property key=EnablePaletteMode advertised=false set_true_status=-12900
hevc_scc_property key=PaletteMode advertised=false set_true_status=-12900
hevc_scc_property key=EnableIntraBlockCopy advertised=false set_true_status=-12900
hevc_scc_property key=IntraBlockCopy advertised=false set_true_status=-12900
hevc_scc_property key=EnableIBC advertised=false set_true_status=-12900
hevc_scc_property key=HEVCSCC advertised=false set_true_status=-12900
```

This output does not prove that the hardware lacks internal screen-content optimization.
It proves that this SDK and encoder session expose no controllable SCC, palette, or IBC property.

### iPad Simulator witness

The probe was an arm64 iOS Simulator executable.
Mach-O `LC_BUILD_VERSION` reported platform 7, minimum iOS 17.0, and SDK 26.4.
The probe ran in the iPad Pro 11-inch (M5) Simulator.

VideoToolbox created valid H.264 and HEVC samples.
The installed `ffmpeg` `libsvtav1` encoder created a valid AV1 `av01` MP4 sample.
The probe also created an uncompressed BGRA sample.
It enqueued all four samples in separate `AVSampleBufferDisplayLayer.sampleBufferRenderer` instances.

The build and run commands were:

```sh
UDID=E99582C8-C7E7-469C-AD42-7CCE4727F2C9
APP=/tmp/HoloirohCodecProbe.app
SDK="$(xcrun --sdk iphonesimulator --show-sdk-path)"

xcrun simctl boot "$UDID"
xcrun simctl bootstatus "$UDID" -b
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i 'testsrc2=size=160x96:rate=1:duration=1' \
  -frames:v 1 -c:v libsvtav1 -preset 12 -pix_fmt yuv420p \
  -movflags +faststart /tmp/av1-probe.mp4

rm -rf "$APP"
mkdir -p "$APP"
SDKROOT="$SDK" xcrun --sdk iphonesimulator swiftc -O -parse-as-library \
  -sdk "$SDK" -target arm64-apple-ios17.0-simulator \
  -framework UIKit -framework AVFoundation -framework VideoToolbox \
  -framework CoreMedia -framework CoreVideo \
  ios/Probes/VideoToolboxSimulatorProbe.swift \
  -o "$APP/HoloirohCodecProbe"
cp /tmp/av1-probe.mp4 "$APP/av1-probe.mp4"
/usr/libexec/PlistBuddy \
  -c 'Add :CFBundleIdentifier string com.holoiroh.CodecProbe' \
  -c 'Add :CFBundleExecutable string HoloirohCodecProbe' \
  -c 'Add :CFBundleName string HoloirohCodecProbe' \
  -c 'Add :CFBundlePackageType string APPL' \
  -c 'Add :CFBundleVersion string 1' \
  -c 'Add :CFBundleShortVersionString string 1.0' \
  -c 'Add :MinimumOSVersion string 17.0' \
  "$APP/Info.plist"
xcrun simctl install "$UDID" "$APP"
xcrun simctl launch --console-pty --terminate-running-process \
  "$UDID" com.holoiroh.CodecProbe
```

The relevant output was:

```text
probe=VideoToolboxSimulatorProbe model=iPad system=iPadOS-26.4.1 arch=arm64
hardware_decode codec=H.264 supported=false
hardware_decode codec=HEVC supported=false
hardware_decode codec=AV1 supported=false
sample_create name=H.264-compressed status=0 sample=true
sample_create name=HEVC-compressed status=0 sample=true
sample_create name=BGRA-uncompressed status=0 sample=true
sample_create name=AV1-compressed status=0 sample=true
display_layer name=H.264-compressed status=rendering ready_for_more=true requires_flush=false error=nil
display_layer name=HEVC-compressed status=rendering ready_for_more=true requires_flush=false error=nil
display_layer name=BGRA-uncompressed status=rendering ready_for_more=true requires_flush=false error=nil
display_layer name=AV1-compressed status=unknown ready_for_more=true requires_flush=false error=nil
```

The Simulator accepted the H.264, HEVC, and uncompressed BGRA samples.
Their renderer status changed to `rendering`.
That status alone does not prove that the Simulator displayed the samples.
The AV1 renderer did not fail.
It stayed in `unknown` state after five seconds.
This result is not an AV1 render pass.

All three `VTIsHardwareDecodeSupported` results were false in Simulator.
The host Mac returned true for all three codecs.
This difference shows why Simulator cannot represent an iPhone or iPad hardware decoder.
The automated substitute proves format-description creation and compressed-sample enqueue.
It also proves renderer state handling.
It does not prove physical-device hardware decode.

### Current integration surface

`cargo metadata` resolves production media crates from `../holoiroh-vendor/iroh-live-patched`.
It does not resolve them from the old Cargo Git checkout cited in 2026-07.
The resolved daemon and bridge feature graphs contain `h264` and `videotoolbox`.
They do not contain `av1`.

The present end-to-end path is H.264-only at these boundaries:

1. `rusty-codecs/src/codec.rs` exposes software H.264, software AV1, and
   VideoToolbox H.264 encoder variants. It has no HEVC encoder variant.
2. `moq-media/src/publish.rs` maps the production encoder selection to
   `VtbEncoder`. The rendition name is `video/h264-vtb-<preset>`.
3. `rusty-codecs/src/codec/vtb/encoder.rs` hard-codes `avc1` and H.264 Baseline.
   It parses H.264 network abstraction layer (NAL) keyframes.
   It extracts sequence parameter sets (SPS) and picture parameter sets (PPS).
   It also creates `avcC` metadata.
   It passes no encoder specification.
   Production therefore permits a software fallback.
   Its `is_hardware()` result is a compile-time enum label.
   It is not a runtime `UsingHardwareAcceleratedVideoEncoder` query.
   The focused probe proves that a hardware-required H.264 session succeeds on this Mac.
4. `rusty-codecs/src/config.rs` models H.264 and AV1. The resolved `hang`
   catalog can model H.265, but the conversion currently changes H.265 to an
   unsupported `Other` string.
5. `rusty-codecs/src/codec/dynamic.rs` dispatches H.264 and optional software
   AV1. It has no HEVC decoder arm.
6. `rusty-codecs/src/codec/vtb/decoder.rs` accepts only H.264. It creates the
   format with `CMVideoFormatDescriptionCreateFromH264ParameterSets` and needs
   SPS/PPS or `avcC` data.
7. `ios-bridge/src/lib.rs` asks the dynamic decoder for decoded frames. The
   patched library selects VideoToolbox H.264 on iOS. The bridge then copies
   decoded BGRA to Swift.
8. `VideoRenderView.swift` receives decoded BGRA pixel buffers. It wraps them
   as uncompressed sample buffers. Production Swift does not receive the
   compressed H.264 track.

An HEVC change needs a VideoToolbox HEVC encoder variant and H.265 catalog mapping.
It needs video parameter set (VPS), SPS, and PPS extraction.
It also needs `hvcC` metadata and H.265 keyframe parsing.
It needs an HEVC dynamic decoder arm and an iOS VideoToolbox HEVC format path.
An AV1 change needs a hardware encoder that this Mac does not have.
Do not implement either switch before both ends pass on physical target hardware.

`BENCHMARKS.md` records the measured H.264 and HEVC results in its codec section.
