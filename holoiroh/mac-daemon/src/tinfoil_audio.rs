//! Audio transcription and text-to-speech via Tinfoil (docs.tinfoil.sh/guides/processing-audio).
//!
//! **Scope invariant (explicit user decision, this session): this module only ever transcribes
//! audio the client captured from its own microphone.** It is an OPT-IN alternative to the
//! default on-device transcription path (`VoiceTranscriber.swift`'s on-device Speech
//! framework) -- never a system/speaker-output capture, which could contain other call
//! participants' voices without their consent. This module has no way to verify audio
//! provenance once bytes arrive (any `Vec<u8>` looks the same); the invariant is enforced at
//! the only place it CAN be enforced -- the iOS client only ever taps its own
//! `AVAudioEngine.inputNode` (mic) for anything routed here, never system audio. See
//! `tinfoil-audio-consent-scope` (PRD).
//!
//! Direct-reqwest-with-bearer style, matching [`crate::tinfoil_documents`]/
//! [`crate::tinfoil_vision`] -- not routed through [`crate::tinfoil_proxy`]'s loopback proxy.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_models::{QWEN3_TTS, TINFOIL_BASE_URL, VOXTRAL_SMALL_24B};

const TRANSCRIPTIONS_PATH: &str = "/v1/audio/transcriptions";
const SPEECH_PATH: &str = "/v1/audio/speech";

#[derive(Deserialize)]
struct TranscriptionResponse {
    #[serde(default)]
    text: String,
}

/// Transcribes `audio_bytes` (the client's own mic capture; see module doc) via Tinfoil's
/// voxtral-small-24b. `filename` only affects the multipart part's declared name -- Tinfoil
/// infers format from content, matching `/v1/convert/file`'s own "automatic format detection."
pub async fn transcribe(api_key: &str, audio_bytes: Vec<u8>, filename: &str) -> Result<String> {
    if audio_bytes.is_empty() {
        bail!("transcribe called with 0 bytes of audio");
    }

    let part = reqwest::multipart::Part::bytes(audio_bytes).file_name(filename.to_string());
    let form = reqwest::multipart::Form::new()
        .text("model", VOXTRAL_SMALL_24B)
        .part("file", part);

    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(60),
        client
            .post(format!("{TINFOIL_BASE_URL}{TRANSCRIPTIONS_PATH}"))
            .header("authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send(),
    )
    .await
    .context("tinfoil audio transcription timed out")?
    .context("tinfoil audio transcription request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("tinfoil {TRANSCRIPTIONS_PATH} returned {status}: {body}");
    }

    let parsed: TranscriptionResponse = response
        .json()
        .await
        .context("failed to parse tinfoil transcription response")?;
    if parsed.text.trim().is_empty() {
        bail!("tinfoil transcription returned empty text");
    }
    Ok(parsed.text)
}

/// Synthesizes `text` as speech via Tinfoil's qwen3-tts, returning raw WAV bytes
/// (docs.tinfoil.sh/guides/processing-audio: "TTS response: WAV audio buffer"). `voice` is
/// passed through verbatim (e.g. `"serena"` per the docs' own example) -- this module does not
/// validate the voice id against a catalog since Tinfoil's own error response already reports
/// an invalid one clearly.
pub async fn speech(api_key: &str, text: &str, voice: &str) -> Result<Vec<u8>> {
    if text.trim().is_empty() {
        bail!("speech called with empty text");
    }

    let body = serde_json::json!({
        "model": QWEN3_TTS,
        "voice": voice,
        "input": text,
    });

    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(60),
        client
            .post(format!("{TINFOIL_BASE_URL}{SPEECH_PATH}"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .json(&body)
            .send(),
    )
    .await
    .context("tinfoil speech synthesis timed out")?
    .context("tinfoil speech synthesis request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("tinfoil {SPEECH_PATH} returned {status}: {text}");
    }

    let bytes = response
        .bytes()
        .await
        .context("failed to read tinfoil speech response body")?;
    if bytes.is_empty() {
        bail!("tinfoil speech synthesis returned 0 bytes");
    }
    Ok(bytes.to_vec())
}

/// Base64-encodes `speech`'s WAV output, the shape `holoiroh_wire::ServerMessage::SpeechReady`
/// carries over the control channel to the iOS client.
pub fn encode_speech_base64(wav_bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(wav_bytes)
}
