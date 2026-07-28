//! Document processing: converts a PDF/DOCX/PPTX/XLSX/HTML/CSV/image file to markdown via
//! Tinfoil's `/v1/convert/file` endpoint (docs.tinfoil.sh/guides/document-processing).
//!
//! Same posture as [`crate::clarify`]: a direct `reqwest` call carrying the real bearer key,
//! not routed through [`crate::tinfoil_proxy`]'s loopback proxy. The proxy exists solely to
//! solve `holo serve`'s inability to express an `Authorization: Bearer <key>` header (see that
//! module's doc); this daemon's own native code has no such constraint and just sets the
//! header directly, matching `clarify.rs`.
//!
//! The daemon uses the **direct-conversion** multipart endpoint (`POST /v1/convert/file`)
//! rather than the API-attachment path (`POST /v1/responses` with a base64 `input_file`) --
//! the direct endpoint is the one the docs name as "primary" and avoids a redundant
//! base64-encode-then-decode round trip for what is already binary file data.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_models::TINFOIL_BASE_URL;

const CONVERT_ENDPOINT_PATH: &str = "/v1/convert/file";

/// Tinfoil's documented per-file and per-request caps (docs.tinfoil.sh/guides/document-processing:
/// "up to 10 files per request", "50 MB maximum per file"). Enforced client-side, before the
/// network call, so an oversized/over-count request fails fast with a clear message instead of
/// a slow upload followed by a server-side rejection.
pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_FILES_PER_REQUEST: usize = 10;

/// Which extraction mode to request. Mirrors the five modes docs.tinfoil.sh/guides/document-processing
/// lists under "processing modes"; `Text` (markdown only) is the default the docs describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertMode {
    /// Markdown only. The default.
    Text,
    /// Adds visual descriptions.
    Vision,
    /// Per-page base64 PNGs.
    Images,
    /// Text layer only (no OCR fallback).
    Raw,
    /// Full-page OCR.
    Vlm,
}

impl ConvertMode {
    fn as_query_value(self) -> &'static str {
        match self {
            ConvertMode::Text => "text",
            ConvertMode::Vision => "vision",
            ConvertMode::Images => "images",
            ConvertMode::Raw => "raw",
            ConvertMode::Vlm => "vlm",
        }
    }
}

/// One file to convert: its name (used for server-side format detection by extension) and raw
/// bytes.
pub struct DocumentInput {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// A successfully converted document's markdown content, per the response shape documented at
/// docs.tinfoil.sh/guides/document-processing (`{"document": {"md_content": "..."}, "status":
/// "success", ...}`).
#[derive(Debug, Clone)]
pub struct ConvertedDocument {
    pub markdown: String,
}

#[derive(Deserialize)]
struct ConvertResponseEnvelope {
    document: ConvertResponseDocument,
}

#[derive(Deserialize)]
struct ConvertResponseDocument {
    #[serde(default)]
    md_content: String,
}

/// Converts one or more files to markdown. Validates the client-side size/count limits before
/// making any network call. Unlike [`crate::clarify::generate_clarifying_questions`] (which
/// swallows every failure into an empty result because it sits off a task's critical path),
/// this returns `Result` -- document processing is something a caller explicitly requested and
/// is waiting on, so a failure must be reported, not silently downgraded to "no documents."
pub async fn convert_documents(
    api_key: &str,
    files: &[DocumentInput],
    mode: ConvertMode,
) -> Result<Vec<ConvertedDocument>> {
    if files.is_empty() {
        bail!("convert_documents called with zero files");
    }
    if files.len() > MAX_FILES_PER_REQUEST {
        bail!(
            "{} files requested but Tinfoil's /v1/convert/file caps a request at {} files",
            files.len(),
            MAX_FILES_PER_REQUEST
        );
    }
    for file in files {
        if file.bytes.is_empty() {
            bail!("file '{}' is empty (0 bytes)", file.filename);
        }
        if file.bytes.len() > MAX_FILE_BYTES {
            bail!(
                "file '{}' is {} bytes, exceeding Tinfoil's {}-byte per-file cap",
                file.filename,
                file.bytes.len(),
                MAX_FILE_BYTES
            );
        }
    }

    let mut form = reqwest::multipart::Form::new();
    for file in files {
        let part = reqwest::multipart::Part::bytes(file.bytes.clone())
            .file_name(file.filename.clone());
        form = form.part("files", part);
    }

    let url = format!(
        "{TINFOIL_BASE_URL}{CONVERT_ENDPOINT_PATH}?mode={}",
        mode.as_query_value()
    );

    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(120),
        client
            .post(&url)
            .header("authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send(),
    )
    .await
    .context("tinfoil document conversion timed out")?
    .context("tinfoil document conversion request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("tinfoil /v1/convert/file returned {status}: {body}");
    }

    let raw = response
        .text()
        .await
        .context("failed to read tinfoil /v1/convert/file response body")?;
    parse_convert_response(&raw)
}

/// Parses a `/v1/convert/file` response body. Extracted from [`convert_documents`] as its own
/// pure, synchronous, testable function (no network) so malformed-response handling can be
/// witnessed directly with fixture strings, not only via a live network round trip -- see
/// `verify-malformed-tinfoil-response` (PRD) and `examples/tinfoil_error_handling_probe.rs`.
///
/// The endpoint's real response shape for a multi-file request is not fully pinned down in the
/// docs summary (single-document envelope shown); accepts either a single envelope or an array
/// of them so a multi-file request does not spuriously fail to parse. The single-document shape
/// was confirmed live against the real endpoint (`tinfoil_live_probe.rs`); the array shape
/// remains an educated guess pending a real multi-file live request.
pub fn parse_convert_response(raw: &str) -> Result<Vec<ConvertedDocument>> {
    if let Ok(single) = serde_json::from_str::<ConvertResponseEnvelope>(raw) {
        return Ok(vec![ConvertedDocument {
            markdown: single.document.md_content,
        }]);
    }
    if let Ok(many) = serde_json::from_str::<Vec<ConvertResponseEnvelope>>(raw) {
        return Ok(many
            .into_iter()
            .map(|e| ConvertedDocument {
                markdown: e.document.md_content,
            })
            .collect());
    }

    bail!("tinfoil /v1/convert/file response did not match either expected shape: {raw}");
}
