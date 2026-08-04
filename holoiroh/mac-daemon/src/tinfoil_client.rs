use std::sync::Arc;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;

/// Document conversion can include markdown and per-page image data.
/// The 64 MiB cap permits Tinfoil's documented 50 MiB input plus response overhead.
pub const DOCUMENT_SUCCESS_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
/// Speech responses are raw WAV data. A 32 MiB cap permits several minutes of PCM audio.
pub const SPEECH_SUCCESS_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// Model JSON and text should be small. The 4 MiB cap permits generous protocol overhead.
pub const JSON_SUCCESS_BODY_LIMIT_BYTES: usize = 4 * 1024 * 1024;
/// Error bodies are diagnostic only. The 64 KiB cap prevents an upstream error from consuming
/// capability-sized memory.
pub const HTTP_ERROR_BODY_LIMIT_BYTES: usize = 64 * 1024;
const HTTP_ERROR_TEXT_LIMIT_CHARS: usize = 1024;
const INITIAL_RESPONSE_BODY_CAPACITY_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ResponseBodyLimitError {
    #[error("declared body is {declared} bytes; limit is {limit} bytes")]
    DeclaredTooLarge { declared: u64, limit: usize },
    #[error(
        "streamed body exceeded {limit}-byte limit after retaining {retained} bytes; observed at least {observed_at_least} bytes"
    )]
    StreamTooLarge {
        limit: usize,
        retained: usize,
        observed_at_least: usize,
    },
    #[error("response body stream failed")]
    Stream(#[source] reqwest::Error),
}

/// Collects a response body without retaining bytes beyond `limit`.
///
/// A declared oversized body fails before the response stream is polled. A chunked body fails
/// before the chunk that crosses the limit is copied into the retained buffer.
pub async fn collect_bounded_response_body(
    response: reqwest::Response,
    limit: usize,
) -> std::result::Result<Vec<u8>, ResponseBodyLimitError> {
    let declared_length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| response.content_length());
    if let Some(declared) = declared_length {
        if declared > limit as u64 {
            return Err(ResponseBodyLimitError::DeclaredTooLarge { declared, limit });
        }
    }

    let initial_capacity = declared_length
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(INITIAL_RESPONSE_BODY_CAPACITY_BYTES)
        .min(INITIAL_RESPONSE_BODY_CAPACITY_BYTES)
        .min(limit);
    let mut body = Vec::with_capacity(initial_capacity);
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ResponseBodyLimitError::Stream)?;
        let observed_at_least = body.len().checked_add(chunk.len()).unwrap_or(usize::MAX);
        if observed_at_least > limit {
            return Err(ResponseBodyLimitError::StreamTooLarge {
                limit,
                retained: body.len(),
                observed_at_least,
            });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

/// Collects one capability response under its success cap and the shared error cap.
///
/// HTTP errors include the status and at most 1,024 sanitized text characters. Binary error
/// bodies are replaced with byte-count metadata.
pub async fn collect_tinfoil_response(
    response: reqwest::Response,
    success_limit: usize,
    operation: &str,
) -> Result<Vec<u8>> {
    let status = response.status();
    let limit = if status.is_success() {
        success_limit
    } else {
        HTTP_ERROR_BODY_LIMIT_BYTES
    };

    let body = match collect_bounded_response_body(response, limit).await {
        Ok(body) => body,
        Err(error) => {
            bail!("{operation} returned {status}: response body rejected: {error}");
        }
    };

    if status.is_success() {
        return Ok(body);
    }

    bail!(
        "{operation} returned {status}: {}",
        sanitize_http_error_body(&body)
    )
}

pub fn sanitize_http_error_body(body: &[u8]) -> String {
    let Ok(text) = std::str::from_utf8(body) else {
        return format!("non-text response body ({} bytes)", body.len());
    };
    if body.contains(&0) {
        return format!("non-text response body ({} bytes)", body.len());
    }

    let mut sanitized = String::new();
    let mut sanitized_characters = 0usize;
    let mut pending_space = false;
    let mut truncated = false;
    for character in text.chars() {
        if character.is_control() || character.is_whitespace() {
            pending_space = !sanitized.is_empty();
            continue;
        }
        if pending_space {
            if sanitized_characters == HTTP_ERROR_TEXT_LIMIT_CHARS {
                truncated = true;
                break;
            }
            sanitized.push(' ');
            sanitized_characters += 1;
            pending_space = false;
        }
        if sanitized_characters == HTTP_ERROR_TEXT_LIMIT_CHARS {
            truncated = true;
            break;
        }
        sanitized.push(character);
        sanitized_characters += 1;
    }

    if sanitized.is_empty() {
        return format!("empty or non-printing response body ({} bytes)", body.len());
    }
    if truncated {
        if sanitized_characters == HTTP_ERROR_TEXT_LIMIT_CHARS {
            sanitized.pop();
        }
        sanitized.push('…');
    }
    sanitized
}

pub struct TinfoilClient {
    client: tinfoil::Client,
    bearer: String,
    ground_truth_json: Arc<str>,
}

impl TinfoilClient {
    pub async fn new(api_key: String) -> Result<Self> {
        let client = tinfoil::Client::new_default_with_api_key(api_key.clone())
            .await
            .context("Tinfoil enclave attestation failed")?;
        let ground_truth_json = client
            .secure_client()
            .ground_truth_json()
            .context("Tinfoil verified ground truth was unavailable")?;
        Ok(Self {
            client,
            bearer: format!("Bearer {api_key}"),
            ground_truth_json: Arc::from(ground_truth_json),
        })
    }

    pub fn client(&self) -> &tinfoil::Client {
        &self.client
    }

    pub fn base_url(&self) -> String {
        format!("https://{}", self.client.enclave())
    }

    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    pub fn ground_truth_json(&self) -> Arc<str> {
        self.ground_truth_json.clone()
    }
}
