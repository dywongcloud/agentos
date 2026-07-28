//! Verified Tinfoil model catalog: a single source of truth for the model ids
//! [`crate::tinfoil_documents`], [`crate::tinfoil_vision`], [`crate::tinfoil_audio`], and
//! [`crate::tinfoil_planner`] request, instead of each module duplicating its own string
//! literal (and risking one of them drifting to a stale/misspelled id unnoticed).
//!
//! Every id here is witnessed against `docs.tinfoil.sh` (see each const's doc) or against
//! [`crate::tinfoil_proxy`]/[`crate::clarify`]'s own pre-existing, already-live usage
//! (`kimi-k2-6`, `gpt-oss-120b` -- both confirmed live in this codebase before this module
//! existed). None of these are guessed.

/// Chat/vision/tool-calling model already wired as the rate-limit fallback in
/// [`crate::tinfoil_proxy`]. Not re-exported from here (that module owns its own literal,
/// pre-dating this catalog) -- listed for completeness of "every model id this daemon speaks
/// to Tinfoil with in one place."
pub const KIMI_K2_6: &str = "kimi-k2-6";

/// Default clarifying-questions model, already wired in [`crate::clarify`]. Listed for the
/// same completeness reason as [`KIMI_K2_6`]; `clarify.rs` keeps its own literal.
pub const GPT_OSS_120B: &str = "gpt-oss-120b";

/// Tool-calling / agentic-planning model. Per docs.tinfoil.sh/guides/tool-calling: "most chat
/// models support function calling. GLM-5.2 is recommended for agentic workflows and complex
/// tool calling scenarios." Used by [`crate::tinfoil_planner`].
pub const GLM_5_2: &str = "glm-5-2";

/// Image-input-capable chat model. **Corrected 2026-07-28**: docs.tinfoil.sh's
/// image-processing guide named `qwen3-vl-30b` as one of two vision models, but a live
/// `GET /v1/models` call (`tinfoil_live_probe.rs`) proved that id does not exist (404 "The
/// model does not exist") -- the docs page was wrong/stale. The real catalog's two
/// `multimodal: true` chat models are this and [`KIMI_K2_6`]. Used by
/// [`crate::tinfoil_vision`] as the default (no special request tuning needed, unlike
/// `kimi-k2-6` which requires [`crate::tinfoil_proxy`]'s `apply_kimi_tuning` for its
/// reasoning-token/`guided_json` quirks).
pub const GEMMA_4_31B: &str = "gemma4-31b";

/// Audio transcription model, per docs.tinfoil.sh/guides/processing-audio's own Python
/// example (`client.audio.transcriptions.create(model="voxtral-small-24b", ...)`). Used by
/// [`crate::tinfoil_audio`].
pub const VOXTRAL_SMALL_24B: &str = "voxtral-small-24b";

/// Text-to-speech model, per the same docs page's TypeScript example
/// (`client.audio.speech.create({model: "qwen3-tts", ...})`). Used by
/// [`crate::tinfoil_audio`].
pub const QWEN3_TTS: &str = "qwen3-tts";

/// Shared upstream base URL, mirroring [`crate::tinfoil_proxy::DEFAULT_UPSTREAM`] /
/// [`crate::clarify`]'s own hardcoded `https://inference.tinfoil.sh` -- kept as a separate
/// const here (rather than importing either of those) since neither is a natural owner of a
/// "the constant every Tinfoil-calling module shares" role and this module's whole purpose is
/// to be that owner going forward.
pub const TINFOIL_BASE_URL: &str = "https://inference.tinfoil.sh";
