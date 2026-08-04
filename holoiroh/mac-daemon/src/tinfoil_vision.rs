//! Analyzes images through Tinfoil's image-capable chat completions endpoint.
//!
//! ## Model source
//!
//! `docs.tinfoil.sh/guides/image-processing` names `qwen3-vl-30b` and `gemma-4-31b`.
//! A live probe showed that `qwen3-vl-30b` does not exist.
//! A request for that model returns Hypertext Transfer Protocol (HTTP) status 404.
//! The probe used `GET /v1/models` in `tinfoil_live_probe.rs` on 2026-07-28.
//! The live catalog contained two chat models with `multimodal: true`.
//! They were `gemma4-31b` without a dash and `kimi-k2-6`.
//! The probe confirmed both models against the real endpoint.
//!
//! ## Privacy invariant
//!
//! Every image passes through [`crate::privacy::ocr_and_redact`] on the device before Base64 encoding.
//! This redaction is mandatory.
//! The `privacy-wire-into-tinfoil-proxy` product requirements document (PRD) defines the same privacy control.
//! The existing screenshot fallback path also applies that control.
//!
//! ## Transport
//!
//! This module uses direct `reqwest` requests with bearer authentication.
//! This approach matches [`crate::clarify`] and [`crate::tinfoil_documents`].
//! Requests do not use the [`crate::tinfoil_proxy`] loopback proxy.
//! That proxy exists only because `holo serve` cannot set the required header.

use anyhow::{Context, Result, bail};
use base64::Engine;
use image::DynamicImage;
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_client::{JSON_SUCCESS_BODY_LIMIT_BYTES, collect_tinfoil_response};
use crate::tinfoil_models::{GEMMA_4_31B, KIMI_K2_6};

const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

/// Tinfoil documents an inclusive maximum of 4,096 pixels for each image dimension.
///
/// Source: `docs.tinfoil.sh/guides/image-processing`.
/// The source recommends "resizing images to a maximum of 4096x4096 pixels for optimal performance".
/// This module downscales larger inputs instead of returning an error.
/// Raw images from the app and screenshots commonly exceed the limit.
/// Therefore, callers do not have to resize those inputs.
const MAX_DIMENSION_PX: u32 = 4096;

/// Selects one of the two image-capable chat models confirmed by live `GET /v1/models`.
///
/// [`VisionModel::Gemma431b`] is the default. See the [`Default`] implementation.
/// It requires no special request tuning.
/// `kimi-k2-6` requires [`crate::tinfoil_proxy`]'s `apply_kimi_tuning` for reasoning-token and `guided_json` behavior.
/// This module does not replicate that tuning.
/// Therefore, [`VisionModel::Kimi`] sends a plain request without tuning.
/// That request may be slower or costlier.
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

pub fn parse_image_analysis_response(raw: &[u8]) -> Result<String> {
    let parsed: ChatCompletionResponse =
        serde_json::from_slice(raw).context("failed to parse tinfoil chat/completions response")?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .unwrap_or_default();
    if content.is_empty() {
        bail!("tinfoil chat/completions returned an empty choices/content");
    }
    Ok(content)
}

/// Sends a privacy-filtered image to `model` for analysis with `prompt`.
///
/// Before sending the request, this function performs these steps:
///
/// 1. The function redacts the image.
/// 2. It downscales the image when necessary.
/// 3. It Base64-encodes the image.
///
/// The function returns the model's text answer.
/// It returns errors to the caller through [`Result`].
/// In contrast, [`crate::clarify::generate_clarifying_questions`] suppresses errors.
/// Image analysis is on the critical path of an explicit user request.
/// It is not an off-path, best-effort enrichment.
pub async fn analyze_image(
    transport: &crate::tinfoil_client::TinfoilClient,
    image: &DynamicImage,
    prompt: &str,
    model: VisionModel,
) -> Result<String> {
    let (redacted, redacted_count) = crate::privacy::ocr_and_redact(image).context(
        "on-device PII redaction failed; refusing to send an unredacted image to Tinfoil",
    )?;
    if redacted_count > 0 {
        tracing::info!(
            redacted_count,
            "tinfoil_vision: redacted PII regions before upload"
        );
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

    let client = transport
        .client()
        .http_client()
        .context("Tinfoil verified HTTP client unavailable")?;
    tokio::time::timeout(Duration::from_secs(60), async {
        let response = client
            .post(format!("{}{CHAT_COMPLETIONS_PATH}", transport.base_url()))
            .header("authorization", transport.bearer())
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("tinfoil image analysis request failed")?;

        let raw = collect_tinfoil_response(
            response,
            JSON_SUCCESS_BODY_LIMIT_BYTES,
            "tinfoil /v1/chat/completions image analysis",
        )
        .await?;
        parse_image_analysis_response(&raw)
    })
    .await
    .context("tinfoil image analysis timed out")?
}

/// Downscales `image` while preserving its aspect ratio.
///
/// The result's width and height do not exceed `max_dim`.
/// A dimension equal to `max_dim` is permitted.
/// If both dimensions fit, the function returns a clone equivalent to `to_owned`.
fn downscale_if_needed(image: &DynamicImage, max_dim: u32) -> DynamicImage {
    let (w, h) = (image.width(), image.height());
    if w <= max_dim && h <= max_dim {
        return image.clone();
    }
    image.resize(max_dim, max_dim, image::imageops::FilterType::Lanczos3)
}
