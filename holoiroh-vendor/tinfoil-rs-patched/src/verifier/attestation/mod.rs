//! Attestation verification module
//!
//! Implements the three-step Tinfoil verification process:
//!
//! ## Step 1: Enclave Runtime Verification (Hardware Attestation)
//! Verifies the enclave is running in genuine secure hardware:
//! - Fetch attestation document from `/.well-known/tinfoil-attestation`
//! - Parse SEV-SNP report
//! - Verify AMD certificate chain to hardware root
//! - Extract measurement and TLS fingerprint
//!
//! ## Step 2: Code Integrity Verification (Sigstore)
//! Verifies the source code was built correctly:
//! - Fetch Sigstore bundle from GitHub
//! - Verify GitHub Actions signatures
//! - Extract expected measurements
//!
//! ## Step 3: Consistency Verification
//! Compares source measurement (Sigstore) with enclave measurement (hardware):
//! - If they match, the enclave runs the exact open-source code
//!
//! ## TLS Binding
//! Verifies TLS connection terminates inside the verified enclave:
//! - Compare server TLS cert SPKI hash with attested fingerprint

pub mod constants;
pub mod sev;
pub mod types;

// Re-export public types
pub use types::{
    AttestationDocument, GroundTruth, Measurement, MeasurementError, PredicateType,
    SnpPlatformInfo, SnpPolicy, TcbParts, ValidationOptions, Verification,
};

use super::util::fetch_with_retry;
use crate::error::{Error, Result};
use futures_util::StreamExt;

const MAX_ATTESTATION_BODY_BYTES: usize = 1024 * 1024;

/// Fetch attestation document from an enclave
pub async fn fetch(host: &str) -> Result<AttestationDocument> {
    let url = format!("https://{}/.well-known/tinfoil-attestation", host);

    let response = fetch_with_retry(&url)
        .await
        .map_err(|e| Error::AttestationFetch(format!("HTTP request failed: {}", e)))?;

    if !response.status().is_success() {
        return Err(Error::AttestationFetch(format!(
            "HTTP {}: {}",
            response.status(),
            response
                .status()
                .canonical_reason()
                .unwrap_or("Unknown error")
        )));
    }

    if let Some(content_length) = response.content_length() {
        if content_length > MAX_ATTESTATION_BODY_BYTES as u64 {
            return Err(Error::AttestationFetch(format!(
                "attestation response declared {content_length} bytes; maximum is {MAX_ATTESTATION_BODY_BYTES}"
            )));
        }
    }

    let capacity = response.content_length().unwrap_or(0) as usize;
    let mut body = Vec::with_capacity(capacity);
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk
            .map_err(|e| Error::AttestationFetch(format!("response body read failed: {e}")))?;
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| Error::AttestationFetch("attestation response size overflow".into()))?;
        if next_len > MAX_ATTESTATION_BODY_BYTES {
            return Err(Error::AttestationFetch(format!(
                "attestation response exceeded {MAX_ATTESTATION_BODY_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body)
        .map_err(|e| Error::AttestationFetch(format!("JSON parse failed: {e}")))
}

/// Full verification with AMD certificate chain (Step 1 complete)
///
/// This performs complete hardware attestation verification:
/// - Fetches VCEK from AMD KDS
/// - Validates VCEK → ASK → ARK certificate chain
/// - Verifies report signature against VCEK
///
/// Uses default `ValidationOptions` for production-grade security.
pub async fn verify_full(doc: &AttestationDocument) -> Result<Verification> {
    verify_full_with_options(doc, &ValidationOptions::default()).await
}

/// Full verification with custom validation options.
///
/// Allows customizing policy, TCB, platform, and VMPL requirements.
/// Use `ValidationOptions::default()` for production-grade security.
pub async fn verify_full_with_options(
    doc: &AttestationDocument,
    options: &ValidationOptions,
) -> Result<Verification> {
    match doc.format {
        PredicateType::SevGuestV2 => sev::verify_full_with_options(&doc.body, options).await,
        PredicateType::TdxGuestV2 => Err(Error::UnsupportedFormat(
            "Intel TDX attestation not yet implemented".into(),
        )),
        PredicateType::SnpTdxMultiPlatformV1 => Err(Error::UnsupportedFormat(
            "Multi-platform predicate type is not a valid hardware attestation format".into(),
        )),
        PredicateType::Unknown => Err(Error::AttestationVerification(
            "Unknown attestation format".into(),
        )),
    }
}
