//! Image analysis via Tinfoil's chat/completions endpoint with an image-input-capable model.
//! docs.tinfoil.sh/guides/image-processing names `qwen3-vl-30b` and `gemma-4-31b`, but a live
//! `GET /v1/models` call (`tinfoil_live_probe.rs`, 2026-07-28) proved `qwen3-vl-30b` does not
//! exist (404) -- the docs page was wrong. The real catalog's two `multimodal: true` chat
//! models are `gemma4-31b` (no dash) and `kimi-k2-6`, confirmed live against the real endpoint.
//!
//! Direct-reqwest-with-bearer style, same posture as [`crate::clarify`]/
//! [`crate::tinfoil_documents`] -- not routed through [`crate::tinfoil_proxy`]'s loopback
//! proxy, which exists only to solve `holo serve`'s header limitation.
//!
//! **Every image is redacted on-device via [`crate::privacy::ocr_and_redact`] before it is
//! base64-encoded into the request body.** This is load-bearing, not optional: it is the same
//! privacy control `privacy-wire-into-tinfoil-proxy` (PRD) wires into the existing screenshot
//! fallback path, applied consistently to this new outbound path too.

use anyhow::{Context, Result, bail};
use base64::Engine;
use image::DynamicImage;
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_models::{GEMMA_4_31B, KIMI_K2_6, TINFOIL_BASE_URL};

const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

/// Tinfoil's documented max image dimension (docs.tinfoil.sh/guides/image-processing:
/// "resizing images to a maximum of 4096x4096 pixels for optimal performance"). Enforced as a
/// downscale (never an error) so a caller never has to pre-resize -- oversized inputs are the
/// common case for a raw phone/screenshot capture, not an exceptional one.
const MAX_DIMENSION_PX: u32 = 4096;

/// Which of Tinfoil's two real (`GET /v1/models`-confirmed) image-capable chat models to
/// request. `Gemma431b` is the default (see [`Default`] impl below): it needs no special
/// request tuning, unlike `kimi-k2-6` which requires [`crate::tinfoil_proxy`]'s
/// `apply_kimi_tuning` for its reasoning-token/`guided_json` quirks -- tuning this module does
/// not replicate, so `Kimi` here sends the plain untuned request and may be slower/costlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionModel {
    Gemma431b,
    Kimi,
}

impl Default for VisionModel {
    fn default() -> Self {
        VisionModel::Gemma431b
    }
}

impl VisionModel {
    fn as_model_id(self) -> &'static str {
        match self {
            VisionModel::Gemma431b => GEMMA_4_31B,
            VisionModel::Kimi => KIMI_K2_6,
        }
    }
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
}

/// Redacts, downscales-if-needed, base64-encodes, and asks `model` about `image` per `prompt`.
/// Returns the model's text answer. Unlike [`crate::clarify::generate_clarifying_questions`],
/// errors are surfaced (`Result`) rather than swallowed -- this is a caller-requested action on
/// the critical path of an explicit user request, not an off-path best-effort enrichment.
pub async fn analyze_image(
    api_key: &str,
    image: &DynamicImage,
    prompt: &str,
    model: VisionModel,
) -> Result<String> {
    let (redacted, redacted_count) = crate::privacy::ocr_and_redact(image)
        .context("on-device PII redaction failed; refusing to send an unredacted image to Tinfoil")?;
    if redacted_count > 0 {
        tracing::info!(redacted_count, "tinfoil_vision: redacted PII regions before upload");
    }

    let resized = downscale_if_needed(&redacted, MAX_DIMENSION_PX);

    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        resized
            .write_to(&mut cursor, image::ImageFormat::Png)
            .context("failed to encode redacted image to PNG")?;
    }
    let data_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
    );

    let body = serde_json::json!({
        "model": model.as_model_id(),
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": prompt},
                {"type": "image_url", "image_url": {"url": data_url}},
            ],
        }],
    });

    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(60),
        client
            .post(format!("{TINFOIL_BASE_URL}{CHAT_COMPLETIONS_PATH}"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .json(&body)
            .send(),
    )
    .await
    .context("tinfoil image analysis timed out")?
    .context("tinfoil image analysis request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("tinfoil {CHAT_COMPLETIONS_PATH} returned {status}: {text}");
    }

    let parsed: ChatCompletionResponse = response
        .json()
        .await
        .context("failed to parse tinfoil chat/completions response")?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    if content.is_empty() {
        bail!("tinfoil chat/completions returned an empty choices/content");
    }
    Ok(content)
}

/// Downscales `image` so neither dimension exceeds `max_dim`, preserving aspect ratio. A no-op
/// (returns a clone via `to_owned`-equivalent) when the image already fits.
fn downscale_if_needed(image: &DynamicImage, max_dim: u32) -> DynamicImage {
    let (w, h) = (image.width(), image.height());
    if w <= max_dim && h <= max_dim {
        return image.clone();
    }
    image.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3)
}
