//! Document processing. This module converts a PDF, DOCX, PPTX, XLSX,
//! HTML, CSV, or image file to markdown. It uses Tinfoil's
//! `/v1/convert/file` endpoint (docs.tinfoil.sh/guides/document-processing).
//!
//! This module uses the same posture as [`crate::clarify`]: a direct
//! `reqwest` call that carries the real bearer key. It does not route
//! through [`crate::tinfoil_proxy`]'s loopback proxy.
//!
//! The proxy exists solely to solve `holo serve`'s inability to express an
//! `Authorization: Bearer <key>` header (see that module's doc). This
//! daemon's own native code has no such constraint. It just sets the
//! header directly, matching `clarify.rs`.
//!
//! The daemon uses the **direct-conversion** multipart endpoint
//! (`POST /v1/convert/file`), rather than the API-attachment path
//! (`POST /v1/responses` with a base64 `input_file`). The docs name the
//! direct endpoint as "primary". The direct endpoint also avoids a
//! redundant base64-encode-then-decode round trip for data that is
//! already binary.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_client::{DOCUMENT_SUCCESS_BODY_LIMIT_BYTES, collect_tinfoil_response};

const CONVERT_ENDPOINT_PATH: &str = "/v1/convert/file";

/// Tinfoil's documented per-file and per-request caps
/// (docs.tinfoil.sh/guides/document-processing: "up to 10 files per
/// request", "50 MB maximum per file").
///
/// The client enforces these caps before the network call. As a result,
/// an oversized or over-count request fails fast with a clear message.
/// This avoids a slow upload followed by a server-side rejection.
pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_FILES_PER_REQUEST: usize = 10;

/// Which extraction mode to request. This enum mirrors the five modes
/// that docs.tinfoil.sh/guides/document-processing lists under
/// "processing modes". `Text` (markdown only) is the default the docs
/// describe.
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

/// One file to convert. It has a name, used for server-side format
/// detection by extension, and raw bytes.
pub struct DocumentInput {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// A successfully converted document's markdown content. This follows the
/// response shape documented at docs.tinfoil.sh/guides/document-processing:
/// `{"document": {"md_content": "..."}, "status": "success", ...}`.
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

/// Converts one or more files to markdown. Validates the client-side size
/// and count limits before making any network call.
///
/// Unlike [`crate::clarify::generate_clarifying_questions`], this function
/// returns `Result`. That function swallows every failure into an empty
/// result, because it sits off a task's critical path. Document
/// processing is different: a caller explicitly requested it and is
/// waiting on it. So a failure must be reported, not silently downgraded
/// to "no documents."
pub fn validate_documents(files: &[DocumentInput]) -> Result<()> {
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
    Ok(())
}

pub async fn convert_documents(
    transport: &crate::tinfoil_client::TinfoilClient,
    files: &[DocumentInput],
    mode: ConvertMode,
) -> Result<Vec<ConvertedDocument>> {
    validate_documents(files)?;

    let mut form = reqwest::multipart::Form::new();
    for file in files {
        let part =
            reqwest::multipart::Part::bytes(file.bytes.clone()).file_name(file.filename.clone());
        form = form.part("files", part);
    }

    let url = format!(
        "{}{CONVERT_ENDPOINT_PATH}?mode={}",
        transport.base_url(),
        mode.as_query_value()
    );

    let client = transport
        .client()
        .http_client()
        .context("Tinfoil verified HTTP client unavailable")?;
    tokio::time::timeout(Duration::from_secs(120), async {
        let response = client
            .post(&url)
            .header("authorization", transport.bearer())
            .multipart(form)
            .send()
            .await
            .context("tinfoil document conversion request failed")?;

        let raw = collect_tinfoil_response(
            response,
            DOCUMENT_SUCCESS_BODY_LIMIT_BYTES,
            "tinfoil /v1/convert/file",
        )
        .await?;
        let raw = std::str::from_utf8(&raw)
            .context("tinfoil /v1/convert/file response was not valid UTF-8")?;
        parse_convert_response(raw)
    })
    .await
    .context("tinfoil document conversion timed out")?
}

/// Parses a `/v1/convert/file` response body. This function is extracted
/// from [`convert_documents`] as its own pure, synchronous, testable
/// function. It performs no network I/O. As a result, tests can witness
/// malformed-response handling directly with fixture strings, not only
/// via a live network round trip. See `verify-malformed-tinfoil-response`
/// (PRD) and `examples/tinfoil_error_handling_probe.rs`.
///
/// The docs summary does not fully pin down the endpoint's real response
/// shape for a multi-file request. It shows only a single-document
/// envelope. This function accepts either a single envelope or an array
/// of them. This way, a multi-file request does not spuriously fail to
/// parse.
///
/// A live request against the real endpoint confirmed the single-document
/// shape (`tinfoil_live_probe.rs`). The array shape remains an educated
/// guess. It is pending a real multi-file live request.
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

    bail!("tinfoil /v1/convert/file response did not match either expected shape");
}
