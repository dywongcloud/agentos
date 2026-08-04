# Capture crate replacement evaluation

- Evaluation date: 2026-08-02
- PRD row: `capture-crate-replacement-eval`
- Production revision inspected: `06e0ef8`
- Decision: **DEFER the replacement**

## Decision

Keep the patched `rusty-capture` path for production.

`screencapturekit` 8.0.1 is suitable for more spike work. It is not suitable for production replacement now.

The decision has four primary causes.

1. The release has a reproducible panic in `SCShareableContent::snapshot()`.
2. The crate does not provide the Holoiroh capture lifecycle or recovery behavior.
3. Its synchronous operations still use an unbounded condition-variable wait.
4. The permission-denied lifecycle could not run in the allowed TCC context.

The candidate has useful properties.

- The license is compatible.
- The direct dependency closure uses permissive licenses.
- The 60 fps callback path worked.
- Normal stop and drop paths exited cleanly.
- The frame can enter the current VideoToolbox API without an application CPU pixel copy.
- The async API did not block the executor thread in the normal path.

These properties do not offset the release panic and migration surface.

## Write boundary

This work added only these source surfaces.

- `CAPTURE_EVALUATION.md`
- `spikes/capture-evaluation/`

The spike has its own `[workspace]` table. It is not a production workspace member.

The root workspace still reports only these members.

```text
holoiroh-daemon
holoiroh-wire
holoiroh-ios-bridge
holoiroh-wire-wasm-demo
```

Witness command:

```sh
cargo metadata --no-deps --format-version 1 \
  --manifest-path /Users/dylanwong/Documents/agentOS/holoiroh/Cargo.toml
```

No production manifest or lock file was changed for this spike.

## Host and toolchain

| Item | Value |
|---|---|
| Mac | `Mac15,6`, Apple M3 Pro |
| Memory | 38,654,705,664 bytes |
| macOS | 26.3.1 (a), build `25D771280a` |
| Rust | 1.95.0, commit `59807616e1fa2540724bfbac14d7976d7e4a3860` |
| Rust host | `aarch64-apple-darwin` |
| Xcode | 26.4.1, build `17E202` |
| Display | index 0, display ID 1 |
| Capture size | 1512 x 982 |
| Pixel format | BGRA |
| Requested rate | 60 fps |
| ScreenCaptureKit queue depth | 8 |

Host witness:

```sh
sw_vers
sysctl -n hw.model
sysctl -n hw.memsize
sysctl -n machdep.cpu.brand_string
rustc -Vv
xcodebuild -version
```

## Candidate provenance and supply chain

### `doom-fish/screencapturekit-rs`

| Item | Verified value |
|---|---|
| Repository | <https://github.com/doom-fish/screencapturekit-rs> |
| Crate | `screencapturekit` |
| Current release tested | 8.0.1 |
| Crate release commit | `2a9f13bcbeadb0aabc5596f0ff3d2ba71da8c1d0` |
| Annotated tag object | `419505f0407e9f4c432b6f9512ed16c8ca4a6b1d` |
| Crate checksum | `9ddaa8d6b16a2762c9a97c9a6297f04cb8ded0487e5ef02dc98b4e2bee3a26c7` |
| Rust version floor | 1.76 |
| License expression | `MIT OR Apache-2.0` |
| Build language | Rust plus a bundled Swift bridge |
| Production version today | 1.5.4 through patched `rusty-capture` |

The 8.0.1 crate manifest includes both license files. It names Per Johansson as the author.

GitHub marks the release commit as verified. The primary repository had no later commit at the review point.

The annotated tag is not signed. The packaged crate matched the immutable release source.

The build script invokes Swift Package Manager. It links a static Swift bridge and Apple frameworks.

The release has this direct runtime dependency shape.

```text
screencapturekit 8.0.1
├── apple-cf 0.9.3
│   └── doom-fish-utils 0.3.3
│       ├── crossbeam-queue 0.3.13
│       │   └── crossbeam-utils 0.8.22
│       └── futures-util 0.3.33
└── apple-metal 0.8.8
    ├── doom-fish-utils 0.3.3
    └── libc 0.2.189
```

Every package in this candidate closure declares MIT, Apache-2.0, or both.

The complete spike lock also contains the current production comparison path. RustSec found no advisory.

```text
Loaded 1186 security advisories
Scanning .../spikes/capture-evaluation/Cargo.lock for vulnerabilities
exit status: 0
```

Witness commands:

```sh
cargo info screencapturekit@8.0.1
cargo tree --manifest-path spikes/capture-evaluation/Cargo.toml \
  -p screencapturekit@8.0.1
cargo audit --file spikes/capture-evaluation/Cargo.lock
```

This supply chain was acceptable for a standalone spike.

Production supply-chain risk is medium-high.

It has a concentration risk. One maintainer family owns the main crate and three direct support crates.

The published source contains about 509 `unsafe` tokens, 50 unsafe implementations, and 72 C ABI declarations.

The Swift package has no remote package dependency. RustSec does not inspect Swift or FFI soundness.

The version ranges are also broad. Version 8.0.1 accepts `apple-cf >=0.6,<0.10` and `apple-metal >=0.6,<0.9`.

### Primary repository activity

Primary GitHub metadata showed active development.

- The repository was pushed on 2026-07-18.
- Release 8.0.1 was published on 2026-07-18.
- GitHub listed 40 releases.
- The preceding year had 453 commits.
- The preceding 90 days had 124 commits.
- Eight GitHub Actions workflows were active.
- An external fix merged 71 minutes after its pull request opened.

The activity has high concentration.

- Maintainer account `1313` had 607 of 668 contributor-attributed commits.
- The share was 96.04% after bot commits were excluded.
- The next non-bot contributor had five contributions.
- Three open pull requests were Dependabot updates.
- All 35 recorded issues were closed.
- The issue close-time median was about 18.7 days.
- The issue close-time p90 was about 312 days.
- External human pull requests merged in 24 of 29 cases.
- Their median merge time was about 12.9 hours.

The release cadence creates API-stability risk.

- Version 1.5.4 was published on 2026-03-09.
- Version 2.0.0 was published on 2026-05-06.
- Version 7.0.0 was published on 2026-06-02.
- Version 8.0.0 was published on 2026-06-19.
- Version 8.0.1 was published on 2026-07-18.
- Published major lines were 1, 2, 3, 5, 6, 7, and 8.
- Version 4.0.0 was tagged but not published.
- The changelog has 15 explicit breaking-change markers.
- Version 1.1.0 included a breaking change in a minor release.

Production still uses 1.5.4. A direct replacement must absorb rapid public API change.

Witness commands:

```sh
gh api repos/doom-fish/screencapturekit-rs
gh api repos/doom-fish/screencapturekit-rs/releases/latest
gh api 'repos/doom-fish/screencapturekit-rs/releases?per_page=100'
gh api 'repos/doom-fish/screencapturekit-rs/contributors?per_page=100'
gh api 'repos/doom-fish/screencapturekit-rs/commits?per_page=100&since=2026-05-04T00:00:00Z'
gh api 'repos/doom-fish/screencapturekit-rs/pulls?state=open&per_page=100'
gh api 'repos/doom-fish/screencapturekit-rs/actions/workflows?per_page=100'
```

Star count was not used as an activity signal.

Primary source paths at release commit `2a9f13bc`:

- `Cargo.toml`: package metadata, features, and dependency ranges.
- `src/shareable_content/mod.rs`: enumeration and synchronous completion.
- `src/shareable_content/snapshot.rs`: batched snapshot implementation.
- `src/screenshot_manager.rs`: single-image snapshot API.
- `src/stream/sc_stream.rs`: handlers, start, stop, and drop.
- `src/async_api.rs`: waker-based content and stream APIs.
- `src/cm/sample_buffer.rs`: frame status, timestamps, and pixel buffer access.
- `src/cm/iosurface.rs`: IOSurface ownership and access.
- `build.rs`: Swift bridge compilation and framework linking.

## Exact public APIs tested

### Enumeration

The synchronous path uses these APIs.

```rust
SCShareableContent::get()
SCShareableContent::displays()
SCShareableContent::windows()
SCShareableContent::applications()
SCShareableContent::snapshot()
```

`get()` returns `SCError` when Screen Recording permission is not available.

The synchronous implementation uses `SyncCompletion::wait()`.

`wait()` calls `Condvar::wait_while()` with no deadline. It has no cancellation token.

The async path uses these APIs.

```rust
AsyncSCShareableContent::get()
AsyncSCStream::new(...)
AsyncSCStream::start_capture()
AsyncSCStream::stop_capture()
AsyncSCStream::try_next()
```

The async implementation uses a `Waker`. It does not block the executor thread.

Its bounded frame queue drops the oldest item when full.

Dropping a control future does not cancel the Apple operation. The operation starts before the future is returned.

If Apple never calls the completion, the raw completion reference stays live. The crate provides no operation deadline.

### Snapshot

The single-image API is:

```rust
SCScreenshotManager::capture_image(&filter, &configuration)
```

It returns a retained `CGImage`. The spike read only its dimensions.

The batched content API is:

```rust
SCShareableContent::snapshot() -> Option<ContentSnapshot>
```

The bridge caps are 64 displays, 4,096 windows, and 1,024 applications.

Each category has a 256 KiB string pool.

The 8.0.1 implementation allocates `Vec::with_capacity()` and then indexes the zero-length vector.

The first access is at `src/shareable_content/snapshot.rs:113`.

```rust
let mut buffer: Vec<MaybeUninit<FFIDisplayData>> = Vec::with_capacity(MAX_DISPLAYS);
...
let d = buffer[i].assume_init();
```

This panics before the candidate can return the documented snapshot.

### Stream

The synchronous stream uses these APIs.

```rust
SCContentFilter::create().with_display(...).with_excluding_windows(&[]).build()
SCStreamConfiguration::new()
    .with_width(...)
    .with_height(...)
    .with_pixel_format(PixelFormat::BGRA)
    .with_fps(60)
    .with_queue_depth(8)
SCStream::new(...)
SCStream::add_output_handler(...)
SCStream::start_capture()
SCStream::stop_capture()
```

`SCStream::drop()` releases the Swift stream and then releases the callback context.

It does not call `stop_capture()` first. The reference-count design protects in-flight callback memory.

The normal runtime drop path stopped callbacks in this spike.

### Frame access

The callback receives an owned `CMSampleBuffer`.

```rust
sample.image_buffer() -> Option<CVPixelBuffer>
buffer.is_backed_by_io_surface()
buffer.as_ptr()
```

The candidate retains CoreMedia and CoreVideo objects. It does not copy pixel bytes for these calls.

## Build evidence

The first build found three API facts that README examples did not make clear.

```text
error[E0609]: no field `displays` on type `Option<ContentSnapshot>`
error[E0308]: `with_width` expected `u32`, found `i32`
error[E0433]: cannot find type `SCScreenshotManager` in this scope
```

The corrected spike then built successfully.

```text
Finished `release` profile [optimized] target(s)
```

The final spike also built the exact current VideoToolbox integration.

The isolated feature set `videotoolbox` alone did not compile. It referenced H.264 modules behind another feature.

```text
error[E0433]: could not find `h264` in `codec`
error[E0432]: could not find `convert` in `processing`
```

The working feature set was `h264` plus `videotoolbox`. This matches the production feature graph.

Final validation:

```sh
cargo fmt --manifest-path spikes/capture-evaluation/Cargo.toml -- --check
cargo clippy --release \
  --manifest-path spikes/capture-evaluation/Cargo.toml \
  --bins -- -D warnings
```

Observed result:

```text
Finished `release` profile [optimized] target(s)
```

## Runtime evidence

### TCC context

The current terminal context had Screen Recording access.

```text
operation=permission_probe
permission_context=allowed
display_count=1
window_count=122
enumeration_ms=43.360
```

No TCC database was changed or reset.

The permission-denied branch could not run in this context.

If enumeration fails, no display exists for `SCContentFilter`. No stream can then be constructed or dropped.

The spike reports this state as `stream_created=false` and `cancellation_reachable=false`.

This statement describes the probe branch. It is not a denied-path runtime witness.

### Enumeration and snapshot

Command:

```sh
spikes/capture-evaluation/target/release/capture-evaluation-spike list
```

Observed output:

```text
operation=enumerate
enumeration_ms=43.371
display_count=1
window_count=122
snapshot_ms=0.183
snapshot_result=panic:index-out-of-bounds
```

The panic hook also reported:

```text
screencapturekit-8.0.1/src/shareable_content/snapshot.rs:113:31
index out of bounds: the len is 0 but the index is 0
```

The single-image API worked.

```text
operation=snapshot
display_index=0
display_count=1
window_count=122
configured_width=1512
configured_height=982
enumeration_ms=39.847
snapshot_ms=97.659
image_width=1512
image_height=982
total_ms=155.333
```

### Benchmark method

The candidate and current path used the same display, size, format, and requested rate.

Each main run used 10 seconds of capture and 500 ms of cancellation observation.

The drop runs used 3 seconds of capture and 500 ms of observation.

The percentile function used the nearest rank at `ceil((n - 1) * p)`.

CPU percent is `(user CPU + system CPU) / wall time * 100`.

RSS is macOS `getrusage(RUSAGE_SELF).ru_maxrss`. macOS reports this value in bytes.

`estimated_missing_callbacks` compares raw callback count with the 60 fps schedule.

This estimate does not claim that Apple dropped a frame.

A static screen can produce a callback with no image buffer. The current wrapper hides such callbacks.

Other repository agents were active during the run. The CPU and RSS values are directional only.

### Candidate, 10-second stop run

Command:

```sh
spikes/capture-evaluation/target/release/capture-evaluation-spike stream 0 10 stop
```

| Metric | Value |
|---|---:|
| Enumeration | 38.779 ms |
| Start call | 45.136 ms |
| First image buffer | 73.040 ms |
| Raw callbacks | 599 |
| Expected callback slots | 601 |
| Estimated missing callback slots | 2 |
| Callback interval p50 | 16.704 ms |
| Callback interval p95 | 17.555 ms |
| Callback interval p99 | 19.892 ms |
| Image buffers | 37 |
| Image-buffer interval p50 | 17.018 ms |
| Image-buffer interval p95 | 3128.852 ms |
| Image-buffer interval p99 | 3260.880 ms |
| Complete callback status | 599 |
| IOSurface-backed image buffers | 37 |
| Existing native-handle adapters | 37 |
| Callbacks after stop | 0 |
| Stop call | 3.815 ms |
| CPU | 0.760% of one core |
| Peak RSS | 17,596,416 bytes |
| Wall time | 10.644076 s |

The raw callback cadence met the requested 60 fps cadence in this run.

The image buffers were sparse when screen content was static. This is not a 60 fps image-throughput witness.

### Candidate, 3-second drop run

| Metric | Value |
|---|---:|
| Enumeration | 56.840 ms |
| Start call | 69.819 ms |
| Raw callbacks | 180 |
| Estimated missing callback slots | 1 |
| Callback interval p50 | 16.711 ms |
| Callback interval p95 | 17.693 ms |
| Callback interval p99 | 17.979 ms |
| Image buffers | 32 |
| Callbacks after drop | 0 |
| Drop call | 0.288 ms |
| CPU | 1.196% of one core |
| Peak RSS | 17,645,568 bytes |

### Candidate async API

Command:

```sh
spikes/capture-evaluation/target/release/async_candidate
```

Observed output:

```text
operation=async_api
permission_context=allowed
display_count=1
window_count=119
async_enumeration_ms=37.003
awaited_start_ms=51.383
frames_before_stop=60
awaited_stop_ms=3.255
frames_after_stop=0
dropped_stop_future=true
frames_after_dropped_stop_future=0
async_stream_drop_ms=0.136
process_exit=clean
```

The normal async stop, dropped stop future, and stream drop paths were clean.

This run does not prove cancellation when an Apple completion never fires.

## Comparison with the current path

The current comparison binary uses the production `rusty-capture` path override.

It resolves `screencapturekit` 1.5.4 and the current patched `rusty-codecs` source.

### Enumeration

```text
implementation=current-rusty-capture
operation=enumerate
display_enumeration_ms=65.663
window_enumeration_ms=30.164
display_count=1
window_count=46
display_0=id:macos-display-1,width:1512,height:982,scale:2
```

The counts are not directly comparable.

The current wrapper filters small and unnamed windows. The candidate count includes all windows.

The current display path also queries `NSScreen` for the Retina scale.

### Current path, 10-second stop run

Command:

```sh
RUST_LOG=rusty_capture=debug \
  spikes/capture-evaluation/target/release/current stream 0 10 stop
```

| Metric | Value |
|---|---:|
| Enumeration | 77.956 ms |
| Construction | 118.510 ms |
| Redundant start call | 24.329 ms |
| First frame after start call | 0.001 ms |
| Image buffers surfaced | 55 |
| Image-buffer interval p50 | 16.609 ms |
| Image-buffer interval p95 | 19.297 ms |
| Image-buffer interval p99 | 3293.041 ms |
| GPU frames | 55 |
| Native CVPixelBuffer handles | 55 |
| Frames surfaced after stop | 8 |
| Stop call | 3.208 ms |
| CPU | 1.240% of one core |
| Peak RSS | 22,003,712 bytes |
| Wall time | 10.724551 s |

The current comparison polls `pop_frame()` every 1 ms. This polling raises its CPU value.

Do not use the CPU difference as a production forecast.

### Live self-healing finding

The trace disproved one current watchdog assumption.

The current callback returns before the heartbeat update when `image_buffer()` is `None`.

A static screen can still deliver 60 callbacks each second with sparse image buffers.

The watchdog therefore treated a live static stream as dead three times in 10 seconds.

Trace excerpt:

```text
screen capture delivered no frames ... rebuilding gap_ms=3221
screen capture stream rebuilt successfully, resuming
screen capture delivered no frames ... rebuilding gap_ms=3034
screen capture stream rebuilt successfully, resuming
screen capture delivered no frames ... rebuilding gap_ms=3049
screen capture stream rebuilt successfully, resuming
```

The explicit stop then surfaced eight more frames during the 500 ms observation window.

A replacement must not copy this false-positive behavior.

Use every valid callback as the liveness heartbeat. Use image buffers only for frame delivery.

An intentional stop must also suppress recovery. An in-flight rebuild must not replace a stopped stream.

### Current path, 3-second drop run

| Metric | Value |
|---|---:|
| Enumeration | 70.125 ms |
| Construction | 114.221 ms |
| Start call | 31.695 ms |
| Frames surfaced | 31 |
| Image-buffer interval p50 | 16.886 ms |
| Image-buffer interval p95 | 18.099 ms |
| Image-buffer interval p99 | 18.524 ms |
| GPU and native-handle frames | 31 |
| Drop call | 0.001 ms |
| Process exit | Clean |
| CPU | 1.290% of one core |
| Peak RSS | 21,184,512 bytes |

## Zero-copy result

### Mac capture to VideoToolbox

The candidate can preserve the current application-level no-copy Mac encode handoff.

The spike performed this adapter on every candidate image buffer.

```rust
let apple = unsafe {
    AppleGpuFrame::from_raw(buffer.as_ptr(), width, height, GpuPixelFormat::Bgra)
};
let frame = VideoFrame::new_gpu(GpuFrame::new(Arc::new(apple)), timestamp);
assert!(frame.native_handle().is_some());
```

`AppleGpuFrame::from_raw` retains the `CVPixelBuffer`. It does not map or copy pixel bytes.

The 10-second run produced 37 image buffers. All 37 produced IOSurface and native CVPixelBuffer handles.

The current `VtbEncoder::push_frame` checks `NativeFrameHandle::CvPixelBuffer`.

It retains the same pixel buffer and gives it to `VTCompressionSession`.

VideoToolbox receives the same pixel-buffer handle. The application CPU fallback is not used for this frame type.

This proves an application-level handoff without a CPU pixel copy.

It does not prove a true end-to-end zero-copy encode.

Apple does not guarantee that VideoToolbox avoids internal staging. BGRA-to-YUV conversion and scaling still occur.

### Metal and end-to-end limits

The candidate exposes IOSurface and Metal texture helpers. The current encoder does not need a Metal texture.

The media stream cannot be end-to-end zero-copy. VideoToolbox must produce compressed H.264 bytes for iroh.

The iOS decode and bridge path also has copies.

1. VideoToolbox outputs an NV12 `CVPixelBuffer`.
2. `rgba_image()` reads it back and converts it.
3. The bridge changes RGBA to BGRA.
4. The bridge copies into Swift memory.
5. Swift copies into a pooled `CVPixelBuffer`.

Changing the capture crate does not remove these decoder-side copies.

## Current Holoiroh patch inventory

The production pin is `n0-computer/iroh-live` commit
`5f95758fcd1450e443a9134c9d9342bcc3957b85`.

The current path override includes five packages.

- `iroh-live`
- `moq-media`
- `rusty-codecs`
- `rusty-capture`
- `iroh-moq`

`iroh-moq` is byte-identical to the pin.

### `rusty-capture` changes

The port must preserve or replace these behaviors.

1. Select a display by stable display ID after CLI index resolution.
2. Prefer the primary display when no index is supplied.
3. Return clear errors for an empty list or invalid index.
4. Support the explicit 60 fps `ScreenConfig`.
5. Include the cursor and request BGRA.
6. Use ScreenCaptureKit queue depth 8.
7. Use a bounded frame channel of two items.
8. Drain to the newest frame in `pop_frame()`.
9. Retain the `CVPixelBuffer` in `AppleGpuFrame`.
10. Update actual dimensions from every delivered image buffer.
11. Preserve window ID selection and Retina scale calculation.
12. Drain stale frames when a subscriber restarts the source.
13. Call `start_capture()` after a prior subscriber stopped the source.
14. Tolerate the first already-running error.
15. Tolerate double stop and teardown races.
16. Keep expensive enumeration off the frame-delivery thread.
17. Never block `pop_frame()` behind a rebuild.
18. Retain the original display or window target for rebuild.
19. Re-resolve that target after stream death.
20. Recompute dimensions and window scale during rebuild.
21. Start the replacement before replacing the old live slot.
22. Keep the old stream until a replacement starts successfully.
23. Retry unavailable locked targets.
24. Signal watchdog shutdown on drop.

The current timing values are:

- Liveness threshold: 3 seconds.
- Watchdog tick: 250 ms.
- Rebuild retry interval: 3 seconds.

Do not port the current heartbeat test without correction.

The replacement needs an explicit lifecycle state.

```text
Running | IntentionallyStopped | Rebuilding(generation) | ShuttingDown
```

Only `Running` can start a rebuild. A generation check must reject stale rebuild results.

The replacement also needs bounded waits. Version 8.0.1 does not provide them.

### Production permission behavior

The daemon performs its own preflight before it creates the media stream.

- `CGPreflightScreenCaptureAccess` checks Screen Recording.
- `AXIsProcessTrusted` checks Accessibility.
- The daemon gives exact System Settings instructions.
- The daemon requires a restart after a grant.
- `--preflight` opens a real capturer for two seconds.

The candidate crate does not replace this daemon behavior.

Secure login input is separate from capture recovery.

The daemon checks `IsSecureEventInputEnabled` every two seconds. ScreenCaptureKit cannot capture protected password fields.

### `rusty-codecs`, `moq-media`, and iOS changes

A capture-only replacement must keep these adjacent patches.

1. Keep VideoToolbox and Apple GPU support enabled on macOS and iOS.
2. Keep `metal-import` macOS-only until an iOS Metal renderer exists.
3. Preserve the native CVPixelBuffer fast path in `VtbEncoder`.
4. Preserve monotonic capture presentation timestamps.
5. Clamp repeated or backward timestamps to one frame interval.
6. Preserve source aspect ratio for the 720p rendition.
7. Preserve the 2 ms empty-source backoff.
8. Preserve the 100 ms presentation-time pacing clamp.
9. Preserve the 20 ms synchronized jitter buffer.
10. Preserve the explicit 60 ms incomplete-group tolerance.
11. Preserve iOS foreground source restart.
12. Preserve the four-second iOS frame liveness check.
13. Preserve full bridge and ticket reconnect when restart fails.
14. Preserve BGRA at the bridge and Swift boundary.

The production 720p encoder output is 30 fps. The capture source is 60 fps.

The faster source reduces phase delay before the 30 fps encoder poll.

### Existing recovery limits

A replacement must state these policies explicitly.

- Initial startup while locked still fails.
- A disconnected display does not fail over to another display.
- A closed window does not match by title or application.
- A live resolution change does not force stream reconstruction.
- `SCShareableContent` can wait forever.
- Stream start and stop can wait forever.
- Watchdog thread creation failure is currently ignored.
- Watchdog shutdown does not join a blocked thread.
- Secure input cannot be captured or reconstructed.

## `libark/apple-media-rs`

GitHub redirects `libark/apple-media-rs` to `rust-media/apple-media-rs`.

| Item | Verified value |
|---|---|
| Repository | <https://github.com/rust-media/apple-media-rs> |
| Exact HEAD | `991da1063d833ba9744f4878b181e2912d914d38` |
| HEAD date | 2026-06-08 UTC |
| Git tags | 0 |
| GitHub releases | 0 |
| License | `MIT OR Apache-2.0` |
| ScreenCaptureKit crate | `screen-capture-kit` 0.7.1 |
| VideoToolbox crate | `video-toolbox` 0.3.1 |
| Declared Rust version floor | None |
| Committed lock file | None |

The repository has both `LICENSE-MIT` and `LICENSE-APACHE`.

All relevant crates are pre-1.0. One crates.io owner publishes every package.

### Activity and bus factor

The default branch had 32 commits at the review point.

- `libark` authored 31 commits.
- One other contributor authored one commit.
- The repository had no commits in 2025.
- Work resumed in January, February, and June 2026.
- The longest observed commit gap was 396 days.
- The repository has no GitHub Actions workflow.
- ScreenCaptureKit runs zero tests.

External response was inconsistent.

- One issue received a response in about 13 hours but stayed open for about 598 days.
- One issue received a response after about nine days and remained open.
- Two external pull requests closed without review comments.
- One pull request waited more than 400 days for closure.
- One external pull request merged after 18 days.

The effective bus factor is one.

### API coverage

The ScreenCaptureKit crate wraps 7 of 15 classes in the macOS 26.4 headers.

It wraps 2 of 4 protocols.

It includes these basic surfaces.

- Shareable displays, windows, and applications.
- Basic content filters.
- Basic stream configuration.
- Stream start and stop.
- Screen and system-audio callbacks.

It omits these relevant surfaces.

- `SCScreenshotManager`.
- `SCContentSharingPicker`.
- `SCRecordingOutput`.
- Microphone output and microphone device selection.
- Current HDR and dynamic-range presets.
- Current frame-information keys.
- Complete availability checks.

Primary source paths at commit `991da106`:

- `screen-capture-kit/src/shareable_content.rs`
- `screen-capture-kit/src/stream/configuration.rs`
- `screen-capture-kit/src/stream/stream.rs`
- `screen-capture-kit/src/stream/output_type.rs`
- `core-media/src/sample_buffer.rs`
- `core-video/src/pixel_buffer_io_surface.rs`
- `video-toolbox/src/compression_session.rs`

### Source defects

Compilation cannot detect Objective-C selector and ABI defects.

The source review found these defects.

1. The cursor property uses an incorrect Objective-C selector.
2. The `sampleRate` property uses `f64`. Apple declares `NSInteger`.
3. The stream-output type has an incorrect Objective-C integer encoding.
4. Safe constructors expose Apple constructors that are unavailable.
5. Two stream paths force-unwrap nullable error pointers.
6. One CoreVideo getter appears to over-release a borrowed color-space value.
7. The VideoToolbox pixel-buffer-pool getter does not check for null.
8. Safe callback APIs accept closures without `Send` or `Sync` bounds.
9. Pixel-buffer base-address locking has no RAII guard.

The repository compiled despite these defects.

```text
cargo check --workspace --all-targets: PASS
cargo test --workspace --all-targets: PASS, 0 tests
cargo clippy -p screen-capture-kit --all-targets -- -D warnings: PASS
```

### Component runtime probes

The upstream ScreenCaptureKit example ran for ten seconds.

It produced real `420v` two-plane `CVPixelBuffer` values with non-null IOSurfaces.

A separate synthetic VideoToolbox probe used hardware encode.

```text
using_hardware=true
callback status=0 flags=1 encoded_bytes=131
```

These are component probes. They are not an integrated Holoiroh capture-to-iroh spike.

The crate can support zero-copy in principle.

`CMSampleBuffer::get_image_buffer()` returns a retained image buffer.

VideoToolbox can accept that buffer directly and retain it through encode completion.

The upstream example did not prove the Holoiroh BGRA, queue, PTS, H.264, and teardown contracts together.

### Apple media decision

**Reject production adoption now.**

Do not add `apple-media-rs` without a successful integrated spike.

That spike must correct the known selectors and ABI types first.

It must then implement the current `VideoSource` contract and full recovery behavior.

The crate was not added to the production workspace.

Primary witness commands:

```sh
gh repo view libark/apple-media-rs \
  --json nameWithOwner,url,defaultBranchRef,pushedAt,updatedAt
gh api repos/rust-media/apple-media-rs/commits/main
gh api --paginate repos/rust-media/apple-media-rs/tags
gh api --paginate repos/rust-media/apple-media-rs/releases
gh api repos/rust-media/apple-media-rs/contributors?per_page=100

cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo check --workspace --all-features
cargo check -p examples --examples
cargo run -p examples --example screen_capture
cargo run -p examples --example video_encode
```

## Risk comparison

| Risk | Current patched path | `screencapturekit` 8.0.1 | `apple-media-rs` |
|---|---|---|---|
| License | MIT OR Apache-2.0 | MIT OR Apache-2.0 | MIT OR Apache-2.0 |
| Production API adapter | Present | Must be built | Must be built |
| Mac VideoToolbox zero-copy | Application handoff present | Application handoff witnessed; internal copy unknown | Component-only witness |
| Lock/login rebuild | Present, with false-positive bug | Absent | Absent |
| Intentional stop handling | Races with watchdog | Clean in direct normal probe | Not integrated |
| Sync timeout | Absent | Absent | Absent from integrated evidence |
| Async nonblocking API | Absent in wrapper | Present | Callback API only |
| Snapshot API | No batched API | Panics in 8.0.1 | Missing |
| API stability | Pinned vendored commit | Rapid major changes | Pre-1.0, no tags |
| Bus factor | n0 plus local patch ownership | Maintainer-family concentration | One |

## Adoption gates

Reconsider `screencapturekit` only after all gates pass.

1. Upstream fixes the 8.0.1 batched snapshot panic.
2. A new release contains that fix.
3. Sync operations gain deadlines, or Holoiroh uses async operations with explicit timeouts.
4. A denied-TCC process runs enumeration, start, stop, future drop, and stream drop probes.
5. A lifecycle adapter uses explicit intentional-stop and rebuild generations.
6. A static-screen run produces no false rebuild.
7. A forced stream death rebuilds the same display and window.
8. Lock and unlock recovery runs on a real Mac session.
9. Subscriber detach and reattach preserve fresh timestamps.
10. The adapter feeds the current VideoToolbox encoder without CPU readback.
11. The daemon preflight and target-selection errors remain unchanged.
12. The full iroh-live and iOS reconnect witnesses pass.

## Exact reproduction commands

```sh
cargo build --release \
  --manifest-path spikes/capture-evaluation/Cargo.toml --bins

spikes/capture-evaluation/target/release/capture-evaluation-spike list
spikes/capture-evaluation/target/release/capture-evaluation-spike snapshot 0
spikes/capture-evaluation/target/release/capture-evaluation-spike permission-probe 0
spikes/capture-evaluation/target/release/capture-evaluation-spike stream 0 10 stop
spikes/capture-evaluation/target/release/capture-evaluation-spike stream 0 3 drop
spikes/capture-evaluation/target/release/async_candidate
spikes/capture-evaluation/target/release/current list
RUST_LOG=rusty_capture=debug \
  spikes/capture-evaluation/target/release/current stream 0 10 stop
spikes/capture-evaluation/target/release/current stream 0 3 drop

cargo fmt --manifest-path spikes/capture-evaluation/Cargo.toml -- --check
cargo clippy --release \
  --manifest-path spikes/capture-evaluation/Cargo.toml \
  --bins -- -D warnings
cargo audit --file spikes/capture-evaluation/Cargo.lock
```

## Spike source hashes

```text
2f828258276e8f7cb0277860c7235442b4fa54e2587c2b41a7bb6a3c0bf98e2b  Cargo.toml
ea743467ce180450cdc91d629976dbbebaf8bc7a02bea3ec67b8275678525ae3  Cargo.lock
2477e24a3ff62ed32013320b01cddb0162e10c504ba8607d6239c2c86fec18f5  src/main.rs
5522063988a0825cf84d150b8885dd58c5cd69a17825ad56f68349af75450a5d  src/bin/current.rs
6b5e2055401cfd1d947685643b447e26aa6dad61b592746be13b39481e4961f0  src/bin/async_candidate.rs
```

## PRD status

The authoritative root PRD row remains unchanged.

This task permits writes only to this document and the standalone spike directory.

The evidence supports a measured **defer** decision.

The denied-TCC runtime branch remains unwitnessed because the instructions prohibit a TCC reset.
