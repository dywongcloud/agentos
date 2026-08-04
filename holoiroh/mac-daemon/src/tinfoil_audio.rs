//! Provides audio transcription and text-to-speech through Tinfoil.
//!
//! Source: `docs.tinfoil.sh/guides/processing-audio`.
//!
//! ## Scope invariant
//!
//! An explicit user decision in this session limits transcription to the app user's microphone.
//! This path is an opt-in alternative to the default on-device transcription path.
//! That path uses the Speech framework in `VoiceTranscriber.swift`.
//! This module never receives system or speaker-output captures.
//! Such captures could contain other call participants' voices without their consent.
//!
//! This module cannot verify provenance after it receives the bytes.
//! All `Vec<u8>` inputs have the same type.
//! Therefore, the app enforces the invariant where it captures audio.
//! The app routes only its `AVAudioEngine.inputNode` microphone capture here.
//! It never routes system audio here.
//! See the `tinfoil-audio-consent-scope` product requirements document (PRD).
//!
//! ## Transport
//!
//! This module uses direct `reqwest` requests with bearer authentication.
//! This approach matches [`crate::tinfoil_documents`] and [`crate::tinfoil_vision`].
//! Requests do not use the [`crate::tinfoil_proxy`] loopback proxy.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_client::{
    JSON_SUCCESS_BODY_LIMIT_BYTES, SPEECH_SUCCESS_BODY_LIMIT_BYTES, collect_tinfoil_response,
};
use crate::tinfoil_models::{QWEN3_TTS, VOXTRAL_SMALL_24B};

const TRANSCRIPTIONS_PATH: &str = "/v1/audio/transcriptions";
const SPEECH_PATH: &str = "/v1/audio/speech";

/// Base64 encoding expands every three input bytes to four output bytes.
///
/// This limit is the encoded form of the inclusive 32 mebibyte (MiB) Waveform Audio File Format (WAV) limit.
/// Encoded output at or below this limit is permitted.
/// It prevents a second unbounded allocation on the control channel.
pub const SPEECH_BASE64_LIMIT_BYTES: usize = SPEECH_SUCCESS_BODY_LIMIT_BYTES.div_ceil(3) * 4;

#[derive(Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: String,
}

pub fn parse_transcription_response(raw: &[u8]) -> Result<String> {
    let parsed: TranscriptionResponse =
        serde_json::from_slice(raw).context("failed to parse tinfoil transcription response")?;
    if parsed.text.trim().is_empty() {
        bail!("tinfoil transcription returned empty text");
    }
    Ok(parsed.text)
}

/// Returns the Multipurpose Internet Mail Extensions (MIME) type for a validated multipart transcription filename.
///
/// The control channel creates fixed filenames from the wire `format` field.
/// The closed mapping prevents caller-provided extensions from creating arbitrary multipart content types.
pub fn transcription_mime_type(filename: &str) -> Result<&'static str> {
    match filename.rsplit_once('.').map(|(_, extension)| extension) {
        Some("wav") => Ok("audio/wav"),
        Some("m4a") => Ok("audio/mp4"),
        Some("mp3") => Ok("audio/mpeg"),
        Some("aac") => Ok("audio/aac"),
        Some("flac") => Ok("audio/flac"),
        Some("ogg") => Ok("audio/ogg"),
        Some("webm") => Ok("audio/webm"),
        _ => bail!("unsupported transcription filename"),
    }
}

/// Rejects empty transcription audio.
///
/// A nonempty input passes validation without modification.
pub fn validate_transcription_input(audio_bytes: &[u8]) -> Result<()> {
    if audio_bytes.is_empty() {
        bail!("transcribe called with 0 bytes of audio");
    }
    Ok(())
}

/// Transcribes the app user's microphone capture through Tinfoil with `voxtral-small-24b`.
///
/// See the module documentation for the microphone-only invariant.
/// `filename` must have a supported extension.
/// Its extension selects the matching MIME type for the multipart part.
/// This pairing preserves the wire format through the complete upload path.
pub async fn transcribe(
    transport: &crate::tinfoil_client::TinfoilClient,
    audio_bytes: Vec<u8>,
    filename: &str,
) -> Result<String> {
    validate_transcription_input(&audio_bytes)?;

    let mime_type = transcription_mime_type(filename)?;
    let part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name(filename.to_string())
        .mime_str(mime_type)
        .context("invalid transcription MIME type")?;
    let form = reqwest::multipart::Form::new()
        .text("model", VOXTRAL_SMALL_24B)
        .part("file", part);

    let client = transport
        .client()
        .http_client()
        .context("Tinfoil verified HTTP client unavailable")?;
    tokio::time::timeout(Duration::from_secs(60), async {
        let response = client
            .post(format!("{}{TRANSCRIPTIONS_PATH}", transport.base_url()))
            .header("authorization", transport.bearer())
            .multipart(form)
            .send()
            .await
            .context("tinfoil audio transcription request failed")?;

        let raw = collect_tinfoil_response(
            response,
            JSON_SUCCESS_BODY_LIMIT_BYTES,
            "tinfoil /v1/audio/transcriptions",
        )
        .await?;
        parse_transcription_response(&raw)
    })
    .await
    .context("tinfoil audio transcription timed out")?
}

/// Rejects empty or whitespace-only speech text.
///
/// An input that contains non-whitespace text passes validation without modification.
pub fn validate_speech_input(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("speech called with empty text");
    }
    Ok(())
}

/// Synthesizes `text` through Tinfoil with `qwen3-tts`.
///
/// Tinfoil returns raw Waveform Audio File Format (WAV) bytes.
/// Source: `docs.tinfoil.sh/guides/processing-audio`, "TTS response: WAV audio buffer".
/// The request passes `voice` unchanged.
/// For example, Tinfoil documentation uses `"serena"`.
/// This module does not validate the voice identifier against a catalog.
/// Tinfoil's error response reports an invalid voice identifier.
pub async fn speech(
    transport: &crate::tinfoil_client::TinfoilClient,
    text: &str,
    voice: &str,
) -> Result<Vec<u8>> {
    validate_speech_input(text)?;

    let body = serde_json::json!({
        "model": QWEN3_TTS,
        "voice": voice,
        "input": text,
    });

    let client = transport
        .client()
        .http_client()
        .context("Tinfoil verified HTTP client unavailable")?;
    tokio::time::timeout(Duration::from_secs(60), async {
        let response = client
            .post(format!("{}{SPEECH_PATH}", transport.base_url()))
            .header("authorization", transport.bearer())
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("tinfoil speech synthesis request failed")?;

        let bytes = collect_tinfoil_response(
            response,
            SPEECH_SUCCESS_BODY_LIMIT_BYTES,
            "tinfoil /v1/audio/speech",
        )
        .await?;
        if bytes.is_empty() {
            bail!("tinfoil speech synthesis returned 0 bytes");
        }
        Ok(bytes)
    })
    .await
    .context("tinfoil speech synthesis timed out")?
}

/// Base64-encodes the Waveform Audio File Format (WAV) output from [`speech`].
///
/// [`holoiroh_wire::ServerMessage::SpeechReady`] carries this value to the app over the control channel.
/// Inputs of exactly 32 MiB are permitted.
/// Inputs larger than the 32 MiB WAV limit return an empty string.
/// The live speech path enforces the same inclusive limit while streaming the response.
/// Therefore, that path cannot reach the oversized-input branch.
pub fn encode_speech_base64(wav_bytes: &[u8]) -> String {
    if wav_bytes.len() > SPEECH_SUCCESS_BODY_LIMIT_BYTES {
        tracing::warn!(
            byte_count = wav_bytes.len(),
            limit = SPEECH_SUCCESS_BODY_LIMIT_BYTES,
            "refusing to base64-encode oversized tinfoil speech output"
        );
        return String::new();
    }

    let encoded_length = wav_bytes
        .len()
        .checked_add(2)
        .and_then(|length| (length / 3).checked_mul(4));
    if encoded_length.is_none_or(|length| length > SPEECH_BASE64_LIMIT_BYTES) {
        tracing::warn!(
            byte_count = wav_bytes.len(),
            limit = SPEECH_SUCCESS_BODY_LIMIT_BYTES,
            "refusing to base64-encode oversized tinfoil speech output"
        );
        return String::new();
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(wav_bytes);
    if encoded.len() > SPEECH_BASE64_LIMIT_BYTES {
        tracing::warn!(
            byte_count = encoded.len(),
            limit = SPEECH_BASE64_LIMIT_BYTES,
            "refusing oversized base64 tinfoil speech output"
        );
        return String::new();
    }
    encoded
}
