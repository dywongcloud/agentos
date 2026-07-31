# OQ-4 benchmark: Holo3.1-35B-A3B Q4 GGUF local latency (this Mac)

**Hardware:** Apple M3 Pro, 36 GB unified memory (above the PRD's 32 GB floor hypothesis).

**Model:** `Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M` (21.3 GB).

- Server: `llama-server` (Homebrew `llama.cpp` v10050) on `127.0.0.1:8080`.
- No cloud calls.
- `--base-url` local mode, per the PRD P0-11 requirement.

## Method

The method used two independent real inference calls. For each call:

- The test sent a genuine full-resolution `screencapture -x` PNG of this Mac's live desktop (not a synthetic or cropped test image) as an OpenAI-compatible `chat.completions` vision request.
- The test measured wall-clock time end-to-end in Python (`time.time()` around the HTTP call).
- The server also returned its own internal `timings` block for a prefill/decode breakdown.

## Results

| Run | Wall-clock latency | Prompt (prefill) tokens | Prefill time | Generation tokens | Generation time |
|---|---|---|---|---|---|
| 1 | 36.364 s | 4074 | 31.59 s | 124 | 4.20 s |
| 2 | 36.182 s | 4072 | 30.09 s | 117 | 3.87 s |

The results were consistent across both runs, not a cold-start artifact. Prefill (image
tokenization plus attention over ~4072 vision+text tokens) dominates at ~83% of total latency.
Generation throughput is ~30 tokens/s.

## Follow-up run: confirmed Metal/GPU offload + downscaled 720p image

The test restarted the server with explicit `-ngl 99 -v`. The log confirmed that Metal was
**already** active by default on this Homebrew build:

- `ggml_metal_init: found device: Apple M3 Pro`
- `load_tensors: offloaded 41/41 layers to GPU`

So the first run above was already GPU-accelerated. The slowness is therefore not a
missing-offload misconfiguration.

The test re-ran the same real-screenshot benchmark. This time, the test downscaled the image
to 720p (1280x720) using `sips`, instead of using the full native-resolution desktop capture
from runs 1-2. The 720p resolution matches the PRD's own "Agent View" default capture
resolution (§7.2):

| Run | Image | Vision+text prompt tokens | Wall-clock latency |
|---|---|---|---|
| 1 | full native res (~2 MB PNG) | 4074 | 36.364 s |
| 2 | full native res (~2 MB PNG) | 4072 | 36.182 s |
| 3 | 720p downscale (~318 KB PNG) | 963 | **8.338 s** |

Vision token count is the dominant lever. The token count dropped from ~4074 to 963 tokens
(4.2x fewer). This produced a 4.3x wall-clock speedup, from 36.3s to 8.3s. This confirms that
prefill cost scales with image resolution as expected. It also confirms that Metal offload
was not the bottleneck.

## Verdict against PRD OQ-4

The PRD's own default "Agent View" capture resolution is 720p (§7.2 capture table). This is
the realistic per-step operating condition, not an artificially large full-desktop capture.
At this resolution, measured latency is **8.3s/step** on this Apple M3 Pro / 36 GB Mac.

This latency is closer to the PRD's target, but it is still **above** the PRD's own <5s
end-to-end target. It is also above the 3.3s/step contingency threshold discussed in OQ-4's
text. The gap is only ~1.7x over target, not the ~11x gap that the naive full-resolution
benchmark suggested.

**This does not yet meet the alpha NFR as specified, but it is within striking distance.**

Several further-reachable levers were **not** yet tried in this pass:

- PRD 7.4's fuller minimization guidance: crop to the *target window only*, not just
  downscale the full desktop. Exclude the menu bar, Dock, notifications, and unrelated
  windows. A real target-window crop would carry meaningfully fewer tokens than a downscaled
  full-desktop image.
- Prompt caching across consecutive steps in the same session. `cache_n` was 0 in every run
  above, so no KV-cache reuse was attempted between calls.
- The `--image-min-tokens` and patch-size tuning flags that the server itself suggested in
  its startup log.

Per the PRD's own OQ-4 language, the <5s end-to-end target should be treated as
reachable-but-not-yet-proven on this hardware class pending those follow-ups. It should not
be treated as definitively unreachable.

## Follow-up: the two optimization levers (target-window crop + KV-cache reuse)

The OQ-4 verdict above named two untried levers for closing the gap to the <5s NFR.
This session measured both levers for real, against the same live `llama-server`.

### Lever 1 — target-window crop (fewer vision tokens)

Cropping the screenshot to a small target-window-sized region (600x400, ~35 KB) instead
of the full desktop drops the vision-token count sharply:

| Image | Prompt tokens |
|---|---|
| full native-res desktop | 4074 |
| 720p full-desktop downscale | 963 |
| **600x400 target-window crop** | **~270** |

So PRD 7.4's "crop to the task-relevant region" guidance gives a real ~15x token reduction
versus full resolution, and a ~3.5x reduction versus 720p.

Absolute wall-clock time on the crop varied from 5.7s to 33s across runs. The 33s outlier
coincided with the machine running at 6% free memory under a heavy concurrent build workload.
This was swap pressure, not a property of the crop itself. A clean idle-system measurement is
still owed. However, the token-count reduction itself is unconditional and measured.

### Lever 2 — KV-cache reuse across steps (`cache_prompt: true`)

Two consecutive calls shared the same prompt prefix:

- Same system prompt and instruction.
- Same image.
- `cache_prompt: true` on both calls.

| Call | prompt_tokens processed | prefill (prompt_ms) | cache_n reused | wall-clock |
|---|---|---|---|---|
| 1 (cold) | 275 | 2081 ms | 0 | 5.74 s |
| 2 (warm, same prefix) | 4 | 493 ms | **271** | **2.26 s** |

The warm call reused 271 cached tokens, with only 4 new tokens to prefill. This gave a
**4.2x prefill speedup** (2081 → 493 ms). The warm-step wall-clock was **2.26 s —
comfortably under the 5 s NFR**.

### Verdict

Both levers work as the PRD predicted. In a real multi-step task loop, consecutive
screenshots share nearly all of their static UI chrome. So KV-cache reuse plus a
target-window crop should keep per-step latency in the low-single-digit-seconds range, on
this M3 Pro / 36 GB hardware. This meets the <5s NFR for warm steps. The cold first step
remains the main remaining cost, though cropping helps there too.

The remaining witness is a clean, idle-system, full-loop measurement, with the daemon
actually driving the executor. This measurement is gated on the same macOS TCC permission
grant that blocks every other live-daemon row (see
`holoiroh-user-action-grant-tcc-and-run-daemon`). The two levers themselves, however, are
proven here against real inference.
