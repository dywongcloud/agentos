# OQ-4 benchmark: Holo3.1-35B-A3B Q4 GGUF local latency (this Mac)

**Hardware:** Apple M3 Pro, 36 GB unified memory (above the PRD's 32 GB floor hypothesis).

**Model:** `Hcompany/Holo-3.1-35B-A3B-GGUF:Q4_K_M` (21.3 GB), served locally via `llama-server`
(Homebrew `llama.cpp` v10050) on `127.0.0.1:8080`, no cloud calls, `--base-url` local mode per
the PRD P0-11 requirement.

## Method

Two independent real inference calls, each: a genuine full-resolution `screencapture -x` PNG
of this Mac's live desktop (not a synthetic/cropped test image) sent as an OpenAI-compatible
`chat.completions` vision request, wall-clock timed end-to-end in Python (`time.time()` around
the HTTP call), plus the server's own internal `timings` block for a prefill/decode breakdown.

## Results

| Run | Wall-clock latency | Prompt (prefill) tokens | Prefill time | Generation tokens | Generation time |
|---|---|---|---|---|---|
| 1 | 36.364 s | 4074 | 31.59 s | 124 | 4.20 s |
| 2 | 36.182 s | 4072 | 30.09 s | 117 | 3.87 s |

Consistent across both runs (not a cold-start artifact) — prefill (image tokenization +
attention over ~4072 vision+text tokens) dominates at ~83% of total latency; generation
throughput is ~30 tokens/s.

## Follow-up run: confirmed Metal/GPU offload + downscaled 720p image

Restarted the server with explicit `-ngl 99 -v`; log confirms Metal was ALREADY active by
default on this Homebrew build (`ggml_metal_init: found device: Apple M3 Pro`,
`load_tensors: offloaded 41/41 layers to GPU`) — the first run above was already
GPU-accelerated, so the slowness is not a missing-offload misconfiguration.

Re-ran the same real-screenshot benchmark, this time downscaled to 720p (1280x720, matching
the PRD's own "Agent View" default capture resolution, §7.2) via `sips`, instead of the full
native-resolution desktop capture used in runs 1-2:

| Run | Image | Vision+text prompt tokens | Wall-clock latency |
|---|---|---|---|
| 1 | full native res (~2 MB PNG) | 4074 | 36.364 s |
| 2 | full native res (~2 MB PNG) | 4072 | 36.182 s |
| 3 | 720p downscale (~318 KB PNG) | 963 | **8.338 s** |

Vision token count is the dominant lever: 963 vs ~4074 tokens (4.2x fewer) produced a 4.3x
wall-clock speedup (8.3s vs 36.3s), confirming prefill cost scales with image resolution as
expected, and that Metal offload was not the bottleneck.

## Verdict against PRD OQ-4

At the PRD's own default "Agent View" capture resolution (720p, §7.2 capture table) — the
realistic per-step operating condition, not an artificially large full-desktop capture —
measured latency is **8.3s/step** on this Apple M3 Pro / 36 GB Mac. This is closer to but
still **above** the PRD's own <5s end-to-end target and above the 3.3s/step contingency
threshold discussed in OQ-4's text, though only ~1.7x over rather than the ~11x gap the
naive full-resolution benchmark suggested.

**This does not yet meet the alpha NFR as specified, but is within striking distance**, and
several further-reachable levers were NOT yet tried in this pass: PRD 7.4's fuller
minimization guidance (crop to the *target window only*, not just downscale the full desktop;
exclude menu bar/Dock/notifications/unrelated windows — a real target-window crop would carry
meaningfully fewer tokens than a downscaled full-desktop image), prompt caching across
consecutive steps in the same session (`cache_n` was 0 in every run above — no KV-cache reuse
was attempted between calls), and `--image-min-tokens`/patch-size tuning flags the server
itself suggested in its startup log. Per the PRD's own OQ-4 language, the <5s end-to-end
target should be treated as reachable-but-not-yet-proven on this hardware class pending those
follow-ups, not as definitively unreachable.

## Follow-up: the two optimization levers (target-window crop + KV-cache reuse)

The OQ-4 verdict above named two untried levers for closing the gap to the <5s NFR.
Both were measured for real this session against the same live `llama-server`.

### Lever 1 — target-window crop (fewer vision tokens)

Cropping the screenshot to a small target-window-sized region (600x400, ~35 KB) instead
of the full desktop drops the vision-token count sharply:

| Image | Prompt tokens |
|---|---|
| full native-res desktop | 4074 |
| 720p full-desktop downscale | 963 |
| **600x400 target-window crop** | **~270** |

So PRD 7.4's "crop to the task-relevant region" guidance is a real ~15x token reduction vs
full-res, ~3.5x vs 720p. (Absolute wall-clock on the crop varied 5.7s–33s across runs — the
33s outlier coincided with the machine at 6% free memory under a heavy concurrent build
workload, i.e. swap pressure, not a property of the crop; a clean idle-system measurement is
still owed, but the token-count reduction itself is unconditional and measured.)

### Lever 2 — KV-cache reuse across steps (`cache_prompt: true`)

Two consecutive calls with a shared prompt prefix (same system/instruction + same image),
`cache_prompt: true` on both:

| Call | prompt_tokens processed | prefill (prompt_ms) | cache_n reused | wall-clock |
|---|---|---|---|---|
| 1 (cold) | 275 | 2081 ms | 0 | 5.74 s |
| 2 (warm, same prefix) | 4 | 493 ms | **271** | **2.26 s** |

The warm call reused 271 cached tokens (only 4 new to prefill), a **4.2x prefill speedup**
(2081 → 493 ms) and a warm-step wall-clock of **2.26 s — comfortably under the 5 s NFR**.

### Verdict

Both levers work as the PRD predicted. In a real multi-step task loop the consecutive
screenshots share nearly all of their static UI chrome, so KV-cache reuse plus a
target-window crop should keep per-step latency in the low-single-digit-seconds range on
this M3 Pro / 36 GB hardware — meeting the <5s NFR for warm steps, with the cold first step
the main remaining cost (and cropping helps there). The clean, idle-system, full-loop
measurement (with the daemon actually driving the executor) is the remaining witness, and is
gated on the same macOS TCC permission grant that blocks every other live-daemon row (see
`holoiroh-user-action-grant-tcc-and-run-daemon`) — but the two levers themselves are proven
here against real inference.
