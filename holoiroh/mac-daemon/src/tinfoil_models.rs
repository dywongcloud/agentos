//! Verified Tinfoil model catalog. This module is the single source of truth for the model ids
//! that [`crate::tinfoil_documents`], [`crate::tinfoil_vision`], [`crate::tinfoil_audio`], and
//! [`crate::tinfoil_planner`] request. Without this module, each caller would duplicate its own
//! string literal, and risk one drifting to a stale or misspelled id unnoticed.
//!
//! This module witnesses every id here against `docs.tinfoil.sh` (see each const's doc), or
//! against [`crate::tinfoil_proxy`]'s and [`crate::clarify`]'s own pre-existing, already-live
//! usage (`kimi-k2-6` and `gpt-oss-120b` -- both confirmed live in this codebase before this
//! module existed). This module contains no guessed ids.

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
/// image-processing guide named `qwen3-vl-30b` as one of two vision models. A live
/// `GET /v1/models` call (`tinfoil_live_probe.rs`) proved that id does not exist: it returns 404
/// "The model does not exist". The docs page was wrong or stale. The real catalog has two
/// `multimodal: true` chat models: this one and [`KIMI_K2_6`]. [`crate::tinfoil_vision`] uses
/// this model as the default. It needs no special request tuning, unlike `kimi-k2-6`, which
/// needs [`crate::tinfoil_proxy`]'s `apply_kimi_tuning` for its reasoning-token and
/// `guided_json` quirks.
pub const GEMMA_4_31B: &str = "gemma4-31b";

/// Audio transcription model, per docs.tinfoil.sh/guides/processing-audio's own Python
/// example (`client.audio.transcriptions.create(model="voxtral-small-24b", ...)`). Used by
/// [`crate::tinfoil_audio`].
pub const VOXTRAL_SMALL_24B: &str = "voxtral-small-24b";

/// Text-to-speech model, per the same docs page's TypeScript example
/// (`client.audio.speech.create({model: "qwen3-tts", ...})`). Used by
/// [`crate::tinfoil_audio`].
pub const QWEN3_TTS: &str = "qwen3-tts";

/// Shared upstream base URL. This mirrors [`crate::tinfoil_proxy::DEFAULT_UPSTREAM`] and
/// [`crate::clarify`]'s own hardcoded `https://inference.tinfoil.sh`. This module keeps a
/// separate const here, rather than importing either of those, because neither module is a
/// natural owner of "the constant every Tinfoil-calling module shares". This module's whole
/// purpose is to be that owner going forward.
pub const TINFOIL_BASE_URL: &str = "https://inference.tinfoil.sh";
