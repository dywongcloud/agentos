# Open question 4 (OQ-4) benchmark: Holo-3.1-35B-A3B Q4 GGUF local latency (this Mac)

**Hardware:** Apple M3 Pro with 36 GB unified memory. This exceeds the product requirements document's (PRD's) proposed 32 GB minimum.

**Model:** `Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M` (21.3 GB).

- Server: `llama-server` (Homebrew `llama.cpp` v10050) on `127.0.0.1:8080`.
- No cloud calls.
- `--base-url` local mode, per the PRD's priority 0 requirement 11 (P0-11).

## Method

The benchmark used two independent inference calls. Each call used these conditions:

- The test captured this Mac's live desktop with `screencapture -x`.
- The test used a full-resolution Portable Network Graphics (PNG) file without synthetic content or cropping.
- The test sent an OpenAI-compatible `chat.completions` vision request.
- Python measured end-to-end wall-clock time with `time.time()` around the Hypertext Transfer Protocol (HTTP) call.
- The server returned an internal `timings` block with the prefill and decode breakdown.

## Results

| Run | Wall-clock latency | Prompt (prefill) tokens | Prefill time | Generation tokens | Generation time |
|---|---|---|---|---|---|
| 1 | 36.364 s | 4074 | 31.59 s | 124 | 4.20 s |
| 2 | 36.182 s | 4072 | 30.09 s | 117 | 3.87 s |

Both runs produced similar results. This two-run sample does not isolate cold-start effects.
Prefill accounts for approximately 83 percent of total latency.
It includes image tokenization and attention over approximately 4,072 vision and text tokens.
Generation throughput is approximately 30 tokens/s.

## Follow-up run: confirmed Metal/graphics processing unit (GPU) offload + downscaled 720p image

The test restarted the server with explicit `-ngl 99 -v`. The log confirmed that this Homebrew build already enabled Metal:

- `ggml_metal_init: found device: Apple M3 Pro`
- `load_tensors: offloaded 41/41 layers to GPU`

Therefore, the first run already used GPU acceleration. Missing offload did not cause the observed latency.

The test repeated the real-screenshot benchmark with a 720p image. It used `sips` to downscale the image to 1280 x 720.
Runs 1 and 2 used the native-resolution desktop capture. The 720p image matches the PRD's default Agent View resolution in §7.2.

| Run | Image | Vision+text prompt tokens | Wall-clock latency |
|---|---|---|---|
| 1 | full native res (~2 MB PNG) | 4074 | 36.364 s |
| 2 | full native res (~2 MB PNG) | 4072 | 36.182 s |
| 3 | 720p downscale (~318 KB PNG) | 963 | **8.338 s** |

Reducing image resolution correlated with fewer vision tokens and lower latency in this benchmark.
Token count decreased from approximately 4,074 to 963, a 4.2x reduction.
Wall-clock time decreased from 36.3 s to 8.3 s, a 4.3x speedup.
These results do not isolate image resolution from other request-level effects.
Metal was active during the measurement. The benchmark did not isolate graphics processing unit utilization as a bottleneck.

## Verdict against PRD OQ-4

The PRD specifies 720p as the default Agent View resolution in §7.2. This resolution represents the expected per-step condition.
A larger full-desktop capture does not represent that condition. At 720p, this Mac measured **8.3 s/step**.
The Mac was an Apple M3 Pro with 36 GB memory.

This result is closer to the PRD target. However, it remains above the target of less than 5 s end to end.
It also exceeds the 3.3 s/step contingency threshold in OQ-4. The gap is approximately 1.7x, not the full-resolution benchmark's approximately 11x.

**This result does not meet the specified alpha non-functional requirement (NFR). These measurements do not establish whether further optimization can close the gap.**

This pass did not test these available options:

- Crop to the target window only, as specified by PRD §7.4.
- Exclude the menu bar, Dock, notifications, and unrelated windows.
- Reuse the prompt cache across consecutive steps in the same session.
- Tune the server's suggested `--image-min-tokens` and patch-size flags.

The benchmark used `cache_n` 0 for every run. Therefore, the calls did not reuse the key-value (KV) cache.
The later benchmark below measures whether a target-window crop contains fewer tokens than the 720p full-desktop image.

OQ-4 describes the target as reachable but unproved on this hardware class. The recorded measurements do not verify that forecast.
Do not describe the target as reachable or unreachable without the required results.

## Follow-up: the two optimization levers (target-window crop + KV-cache reuse)

The OQ-4 verdict identified two untested options for meeting the NFR of less than 5 s.
This session measured both options against the same live `llama-server`.

### Lever 1 — target-window crop (fewer vision tokens)

The test cropped the screenshot to a 600 x 400 target-window region of approximately 35 KB.
This crop reduced the vision-token count:

| Image | Prompt tokens |
|---|---|
| full native-res desktop | 4074 |
| 720p full-desktop downscale | 963 |
| **600 x 400 target-window crop** | **~270** |

The target-window crop used approximately 15x fewer tokens than full resolution. It used approximately 3.5x fewer tokens than 720p.

Crop wall-clock time ranged from 5.7 s to 33 s. The 33 s result occurred during a heavy concurrent build.
The machine had 6 percent free memory. This coincidence does not isolate the outlier's cause.
A clean idle-system measurement remains required. However, system load does not invalidate the recorded token counts.

### Lever 2 — KV-cache reuse across steps (`cache_prompt: true`)

Two consecutive calls shared the same prompt prefix:

- Same system prompt and instruction.
- Same image.
- `cache_prompt: true` on both calls.

| Call | prompt_tokens processed | prefill (prompt_ms) | cache_n reused | wall-clock |
|---|---|---|---|---|
| 1 (cold) | 275 | 2081 ms | 0 | 5.74 s |
| 2 (warm, same prefix) | 4 | 493 ms | **271** | **2.26 s** |

The warm call reused 271 cached tokens. It processed only four new prefill tokens.
Prefill was **4.2x faster**, decreasing from 2,081 ms to 493 ms.
The 2.26 s wall-clock result used an identical image.
It is a best-case lower bound for changed-screen latency. It is also an upper bound on possible cache speedup.

### Verdict

The target-window crop used fewer visual tokens in this benchmark.
Production also requires a complete coordinate path that preserves click and drag semantics.
The cache result does not prove a similar gain for a real loop. The installed `llama-server` requires identical multimodal chunk identifiers.
In this witness, the changed screenshot did not reuse the previous image embedding.
This build also disables `--cache-reuse` with the multimodal projector.

The daemon-owned local proxy sends `cache_prompt: true`. A new changed-screen witness explains why production does not claim a speedup:

| Request | Screen | `cache_n` | Prompt tokens processed | Prefill | Wall clock |
|---|---|---:|---:|---:|---:|
| 1 | real desktop A | 0 | 1078 | 4907.721 ms | 5205.429 ms |
| 2 | real desktop B with a new foreground overlay | 0 | 1078 | 4966.779 ms | 5258.686 ms |
| 3 | exact repeat of desktop B | 1074 | 4 | 64.802 ms | 319.949 ms |

The first two PNG files came from consecutive ScreenCaptureKit captures. Their bytes differed.
This llama.cpp build reused only the exact multimodal request. It did not reuse the stable text prefix with a changed screenshot.
Therefore, production does not enable `--cache-reuse`, slot persistence, or cache-RAM tuning.
With this build, the multimodal projector prevents the required changing-screen prefix reuse.
Keep `cache_prompt: true` for exact retries.
Treat the 319.949 ms repeat as a best-case latency lower bound. Treat its speedup as an upper bound.

## MLX port decision

Keep the production path on llama.cpp. Do not port Holo-3.1-35B-A3B to MLX without both required witnesses:

- MLX supports the model family.
- A same-hardware benchmark beats the tuned llama.cpp baseline.

The frequently repeated "8.5x faster prefill" result does not apply to this Mac path.
That result came from `mlx-vlm` PR 1423.
The change fixed a Compute Unified Device Architecture (CUDA) fallback. It did not affect Metal behavior.
A repository search found no Holo model support in `mlx-lm` or `mlx-vlm`.
MLX prefix caching does not compensate for this compatibility gap. Its July 2026 implementation also had these open failures:

- Growing cached prompts.
- Text-only mixture-of-experts requests.

Re-evaluate MLX only after all of these conditions are true:

1. `mlx-vlm` supports Holo-3.1 models directly.
2. The relevant prefix-cache failures are fixed.
3. The same Holo quantization, prompt, screenshot, and Apple hardware beat the tuned llama.cpp results above.
   Compare both prefill and end-to-end results.

## Video screen-content codec evaluation

### Decision

Keep hardware H.264. Do not ship High Efficiency Video Coding (HEVC) or AOMedia Video 1 (AV1) in this pass.

HEVC Main used 25.065 percent fewer bytes than H.264 Baseline for this sequence.
It also produced 1.096 dB higher decoded peak signal-to-noise ratio (PSNR).
These measurements apply to HEVC Main. They do not apply to HEVC Screen Content Coding (SCC).
VideoToolbox did not expose SCC, palette, or intra-block-copy controls on this target.

This Mac provided no hardware AV1 encoder for the probe.
A hardware-required AV1 compression session returned `-12908`, `kVTCouldNotFindVideoEncoderErr`.
`VTCopyVideoEncoderList` returned no AV1 encoder.

The resolved Rust media stack and current iOS decoder support only H.264.
The iPad Simulator cannot prove physical-device hardware decoding. These proof and integration gaps exceed the measured HEVC size reduction.

### Target

- MacBook Pro `Mac15,6`
- Apple M3 Pro, 36 GB unified memory
- arm64
- macOS 26.3.1 (a), build `25D771280a`
- Xcode 26.4.1, build `17E202`
- macOS software development kit (SDK) 26.4
- Swift 6.3.1

### Deterministic input

`VideoToolboxCodecBenchmark.swift` generates `deterministic-desktop-v1` in memory. The sequence uses these fixed parameters:

- 1280 x 720 blue, green, red, and alpha (BGRA) frames
- 90 frames at 30 frames per second
- dark window chrome and a fixed side bar
- repeated hard horizontal edges
- short colored code-line blocks
- deterministic vertical scroll movement
- deterministic cursor movement

The probe sends identical generated frames to each codec. It requires hardware acceleration for compression and decompression.
Both encoders use these settings:

- Real-time mode.
- No frame reordering.
- A 30-frame keyframe interval.
- A target average bit rate of 4,000,000 bit/s.

The H.264 run uses Baseline Auto Level. The HEVC run uses Main Auto Level.

The probe measures encode latency from `VTCompressionSessionEncodeFrame` submission to the matching presentation-timestamp callback.
It measures size with `CMSampleBufferGetTotalSampleSize`. It decodes every sample to BGRA through a hardware-required `VTDecompressionSession`.
It compares every red, green, and blue (RGB) component with the source generator.
The correctness metrics are mean absolute error and PSNR.

### Command

```sh
xcrun swiftc -O -target arm64-apple-macosx26.0 \
  -framework VideoToolbox -framework CoreMedia -framework CoreVideo \
  ios/Probes/VideoToolboxCodecBenchmark.swift \
  -o /tmp/holoiroh-vt-codec-benchmark

for run in 1 2 3; do
  printf '%s\n' "--- run=$run ---"
  /tmp/holoiroh-vt-codec-benchmark
done
```

The command produced a Mach-O arm64 executable.

### Results

The 50th percentile (p50) is the median. The 95th percentile (p95) is the value below 95 percent of observations.

All three runs encoded and decoded 90 of 90 frames.
All callback-error counts were zero. All dimension-error counts were zero.

| Run | Codec | Bytes | Actual bit rate | Encode p50 | Encode p95 | Encode max | Sequence time |
|---|---|---:|---:|---:|---:|---:|---:|
| 1 | H.264 Baseline | 880,058 | 2,346,821 bit/s | 6.845 ms | 19.618 ms | 46.682 ms | 571.914 ms |
| 1 | HEVC Main | 659,472 | 1,758,592 bit/s | 8.389 ms | 21.744 ms | 23.535 ms | 543.259 ms |
| 2 | H.264 Baseline | 880,058 | 2,346,821 bit/s | 6.725 ms | 20.731 ms | 42.607 ms | 526.359 ms |
| 2 | HEVC Main | 659,472 | 1,758,592 bit/s | 8.046 ms | 13.304 ms | 23.895 ms | 606.324 ms |
| 3 | H.264 Baseline | 880,058 | 2,346,821 bit/s | 6.673 ms | 6.969 ms | 53.406 ms | 732.574 ms |
| 3 | HEVC Main | 659,472 | 1,758,592 bit/s | 22.400 ms | 24.095 ms | 24.472 ms | 441.528 ms |

The encoded sizes and decoded-pixel metrics remained byte-for-byte identical across all three runs:

| Codec | Mean absolute error | PSNR |
|---|---:|---:|
| H.264 Baseline | 1.2249 | 37.638 dB |
| HEVC Main | 1.1747 | 38.734 dB |

The exact first repeated-run output was:

```text
benchmark codec=H.264-Baseline frames_in=90 frames_encoded=90 frames_decoded=90 bytes=880058 bitrate_bps=2346821 sequence_ms=571.914 encode_p50_ms=6.845 encode_p95_ms=19.618 encode_max_ms=46.682 decode_mae=1.2249 decode_psnr_db=37.638 encode_callback_errors=0 decode_callback_errors=0 dimension_errors=0
benchmark codec=HEVC-Main frames_in=90 frames_encoded=90 frames_decoded=90 bytes=659472 bitrate_bps=1758592 sequence_ms=543.259 encode_p50_ms=8.389 encode_p95_ms=21.744 encode_max_ms=23.535 decode_mae=1.1747 decode_psnr_db=38.734 encode_callback_errors=0 decode_callback_errors=0 dimension_errors=0
```

### Interpretation limits

The probe submits frames without pacing. Sequence time measures encoder throughput for this fixed batch.
It does not measure three seconds of wall-clock playback. Callback latency is the per-frame encode result.

Both codecs used the same rate-control target. Both remained below that target on this synthetic sequence.
HEVC used 220,586 fewer bytes, a 25.065 percent reduction. HEVC also produced slightly better decoded quality.

Do not attribute this reduction to SCC. The HEVC run used Main profile.
The capability probe found no exposed SCC property. A valid SCC claim requires these measurements:

- An accepted SCC property.
- An SCC-on result.
- An SCC-off result.

Do not use the Simulator result as physical-device proof.
The arm64 iPad Simulator returned false from `VTIsHardwareDecodeSupported` for H.264, HEVC, and AV1.
Its display layer rendered valid H.264 samples, valid HEVC samples, and the current uncompressed BGRA sample.
This result proves sample configuration and Simulator renderer acceptance. It does not prove iPhone or iPad hardware decoding.
The AV1 layer remained in `unknown` state after five seconds. It did not report an error.
Treat the AV1 result as inconclusive.

See `mac-daemon/TRANSPORT_ADR.md` for the capability output and the exact
production integration surface.

## Request-time target-window crop overhead

The optimized `window_crop_probe` measured the daemon's local Joint Photographic Experts Group (JPEG) crop path.
The probe ran on this Apple M3 Pro.
The deterministic input was one 2000 x 1000 JPEG of 343,840 bytes. The 800 x 500 crop started at `(400,200)`.
The re-encoded JPEG was 66,402 bytes. The resolver ran 2,000 times.
The complete decode, crop, and re-encode path ran 20 times.

| Stage | p50 | p95 |
|---|---:|---:|
| Deterministic geometry resolver | 83 ns | 84 ns |
| JPEG decode | 7.555 ms | 8.075 ms |
| Crop and JPEG encode | 3.563 ms | 3.629 ms |
| Complete request crop pipeline | 11.274 ms | 11.794 ms |

Command:

```sh
cargo run --release --locked --manifest-path mac-daemon/Cargo.toml \
  --example window_crop_probe
```

These results measure only daemon-side local image processing.
They do not measure llama vision tokens, model latency, or end-to-end loop speed. Do not infer a model speedup from them.

## Llama prefill launch-flag evaluation

The benchmark used the local Holo-3.1-35B-A3B Q4_K_M server.
The tuned configuration used a quantized 8-bit (Q8) key and value (K/V) cache.
Each configuration used these conditions:

- The same real desktop screenshot, 1,280 pixels wide.
- 1,065 prompt tokens.
- Eight requested output tokens.
- Metal offload.
- `cache_prompt: false`.
- Three requests under one newly started server.

| Configuration | Prefill median | Wall-clock median |
|---|---:|---:|
| llama.cpp defaults | 5,080.275 ms | 5,357.470 ms |
| batch 2048, ubatch 2048, flash attention, Q8 K/V cache | 4,866.824 ms | 5,208.556 ms |

Combined tuning decreased median prefill by 4.202 percent. It decreased median wall-clock time by 2.780 percent.
This machine did not reproduce the third-party 2.7x claim.
The combined result does not identify the effect of each flag. Q8 cache also changes numerical behavior.
Therefore, production keeps the llama.cpp defaults.
The local proxy independently enforces these controls:

- `n_predict`.
- `cache_prompt`.
- Loopback confinement.
- Request and response bounds.

## Dense-model routing evaluation

The official Holo-3.1 collection provides dense 0.8B, 4B, and 9B repositories. It provides no official dense GGUF artifact.
A production-compatible spike used the community `mradermacher/Holo-3.1-4B-GGUF:Q4_K_M` conversion. The conversion used its Q8 multimodal projector.
The spike compared this conversion with the official 35B-A3B Q4_K_M GGUF.

Three requests used the same real desktop screenshot and 1,070 prompt tokens. Each request asked for 64 output tokens.
The requests produced these medians:

| Model | Prefill median | Wall-clock median |
|---|---:|---:|
| dense 4B conversion | 4,065.691 ms | 6,234.749 ms |
| 35B-A3B production | 5,778.345 ms | 8,586.180 ms |

The dense conversion was approximately 27 percent faster for this generic request. This result does not justify routing.
A deterministic 2560 x 1440 graphical user interface (GUI) fixture placed a blue CONTINUE button at normalized center `(719,500)`.
The fixture required a `click_desktop` tool call.
The dense 4B conversion emitted 256 literal zeroes in `reasoning_content`. It emitted no tool call after 42,460.216 ms.
The production 35B-A3B model emitted `click_desktop({"x":720,"y":500})` after 39,296.918 ms. This point was one normalized pixel from the target center.

Therefore, production does not add a tiered router. The smaller artifact lacks official GGUF provenance.
It also failed the grounding acceptance case despite its modest latency advantage for the simple request.
Reconsider routing only when an official dense GGUF exists. A representative grounding suite must also match the production model's success rate.
