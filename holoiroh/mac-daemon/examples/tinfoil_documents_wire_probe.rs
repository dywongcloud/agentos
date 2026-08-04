//! Pure-logic CI witness for the document-processing wire messages
//! (`ClientMessage::ProcessDocument` + `ServerMessage::DocumentProcessed`/
//! `DocumentProcessFailed`) and `tinfoil_documents`'s client-side validation. No network call:
//! every case here is rejected (or would be accepted) before `convert_documents` ever builds a
//! request, so this is fully deterministic and CI-safe, same posture as `clarify_wire_probe`.
//!
//!   cargo run --example tinfoil_documents_wire_probe -p holoiroh-daemon

use holoiroh_daemon::control_channel::{ClientMessage, ServerMessage};
use holoiroh_daemon::tinfoil_documents::{
    DocumentInput, MAX_FILE_BYTES, MAX_FILES_PER_REQUEST, validate_documents,
};

#[tokio::main]
async fn main() {
    // --- wire round-trip ---
    let request = ClientMessage::ProcessDocument {
        request_id: "req-1".to_string(),
        filename: "notes.pdf".to_string(),
        data_base64: "AAAA".to_string(),
        mode: "text".to_string(),
    };
    let rj = serde_json::to_string(&request).expect("serialize request");
    assert!(
        rj.contains("\"type\":\"process_document\""),
        "wrong type tag: {rj}"
    );
    let back: ClientMessage = serde_json::from_str(&rj).expect("deserialize request");
    assert_eq!(back, request);

    let ok = ServerMessage::DocumentProcessed {
        request_id: "req-1".to_string(),
        markdown: "# Notes".to_string(),
    };
    let oj = serde_json::to_string(&ok).expect("serialize ok");
    assert!(
        oj.contains("\"type\":\"document_processed\""),
        "wrong type tag: {oj}"
    );
    let back_ok: ServerMessage = serde_json::from_str(&oj).expect("deserialize ok");
    assert_eq!(back_ok, ok);

    let failed = ServerMessage::DocumentProcessFailed {
        request_id: "req-1".to_string(),
        error: "file too large".to_string(),
    };
    let fj = serde_json::to_string(&failed).expect("serialize failed");
    assert!(
        fj.contains("\"type\":\"document_process_failed\""),
        "wrong type tag: {fj}"
    );
    println!("wire round-trip: OK");

    // --- client-side validation (no network reached in any of these) ---
    let empty_files: Vec<DocumentInput> = Vec::new();
    let err = validate_documents(&empty_files)
        .expect_err("zero files must be rejected before any network call");
    println!("zero files -> {err}");

    let too_many: Vec<DocumentInput> = (0..MAX_FILES_PER_REQUEST + 1)
        .map(|i| DocumentInput {
            filename: format!("f{i}.txt"),
            bytes: vec![1, 2, 3],
        })
        .collect();
    let err = validate_documents(&too_many)
        .expect_err("over the file-count cap must be rejected client-side");
    println!("{}-file request -> {err}", too_many.len());

    let empty_file = vec![DocumentInput {
        filename: "empty.txt".to_string(),
        bytes: Vec::new(),
    }];
    let err =
        validate_documents(&empty_file).expect_err("a 0-byte file must be rejected, not sent");
    println!("0-byte file -> {err}");

    let oversized = vec![DocumentInput {
        filename: "huge.bin".to_string(),
        bytes: vec![0u8; MAX_FILE_BYTES + 1],
    }];
    let err = validate_documents(&oversized)
        .expect_err("over the per-file byte cap must be rejected client-side");
    println!("oversized file -> {err}");

    println!(
        "tinfoil_documents_wire_probe: OK -- wire shapes round-trip and every client-side validation rejects before any network call."
    );
}
