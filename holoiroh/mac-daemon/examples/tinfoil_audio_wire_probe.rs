//! Pure-logic CI witness for the audio wire messages (`ClientMessage::TranscribeAudio` +
//! `ServerMessage::AudioTranscribed`/`AudioTranscriptionFailed`, `ClientMessage::RequestSpeech`
//! + `ServerMessage::SpeechReady`/`SpeechFailed`) and `tinfoil_audio`'s empty-input validation.
//! No network call: both `transcribe`/`speech` reject empty input before building a request.
//!
//!   cargo run --example tinfoil_audio_wire_probe -p holoiroh-daemon

use holoiroh_daemon::control_channel::{
    ClientMessage, MAX_CONTROL_FRAME_BYTES, ServerMessage, transcription_filename_for_format,
};
use holoiroh_daemon::tinfoil_audio::{
    encode_speech_base64, transcription_mime_type, validate_speech_input,
    validate_transcription_input,
};
use holoiroh_daemon::tinfoil_documents::MAX_FILE_BYTES;

#[tokio::main]
async fn main() {
    let transcribe_req = ClientMessage::TranscribeAudio {
        request_id: "req-3".to_string(),
        audio_data_base64: "AAAA".to_string(),
        format: "wav".to_string(),
    };
    let rj = serde_json::to_string(&transcribe_req).expect("serialize request");
    assert!(
        rj.contains("\"type\":\"transcribe_audio\""),
        "wrong type tag: {rj}"
    );
    let back: ClientMessage = serde_json::from_str(&rj).expect("deserialize request");
    assert_eq!(back, transcribe_req);

    assert_eq!(transcription_filename_for_format("wav"), Ok("audio.wav"));
    assert_eq!(transcription_mime_type("audio.wav").unwrap(), "audio/wav");
    assert_eq!(
        transcription_filename_for_format("audio/x-wav"),
        Ok("audio.wav")
    );
    assert_eq!(transcription_filename_for_format("M4A"), Ok("audio.m4a"));
    assert_eq!(transcription_mime_type("audio.m4a").unwrap(), "audio/mp4");
    assert_eq!(
        transcription_filename_for_format("audio/mp4"),
        Ok("audio.m4a")
    );
    assert_eq!(
        transcription_filename_for_format("audio/mpeg"),
        Ok("audio.mp3")
    );
    assert!(transcription_filename_for_format("").is_err());
    assert!(transcription_filename_for_format("audio/unknown").is_err());
    assert!(transcription_filename_for_format("../../secret.wav").is_err());
    assert!(transcription_filename_for_format("wav\r\ncontent-type:text/plain").is_err());
    assert!(transcription_mime_type("audio.bin").is_err());

    let largest_document_base64_bytes = MAX_FILE_BYTES.div_ceil(3) * 4;
    assert!(
        MAX_CONTROL_FRAME_BYTES > largest_document_base64_bytes,
        "control-frame cap must admit a 50 MiB document after base64 expansion plus envelope metadata"
    );

    let transcribed = ServerMessage::AudioTranscribed {
        request_id: "req-3".to_string(),
        text: "hello there".to_string(),
    };
    let tj = serde_json::to_string(&transcribed).expect("serialize transcribed");
    assert!(
        tj.contains("\"type\":\"audio_transcribed\""),
        "wrong type tag: {tj}"
    );

    let speech_req = ClientMessage::RequestSpeech {
        request_id: "req-4".to_string(),
        text: "hi".to_string(),
        voice: "serena".to_string(),
    };
    let sj = serde_json::to_string(&speech_req).expect("serialize speech request");
    assert!(
        sj.contains("\"type\":\"request_speech\""),
        "wrong type tag: {sj}"
    );
    let back_s: ClientMessage = serde_json::from_str(&sj).expect("deserialize speech request");
    assert_eq!(back_s, speech_req);

    let ready = ServerMessage::SpeechReady {
        request_id: "req-4".to_string(),
        audio_data_base64: encode_speech_base64(b"fake-wav-bytes"),
    };
    let rdj = serde_json::to_string(&ready).expect("serialize ready");
    assert!(
        rdj.contains("\"type\":\"speech_ready\""),
        "wrong type tag: {rdj}"
    );
    println!("wire round-trip: OK");

    // Empty-input client-side validation -- no network reached in either case.
    let err = validate_transcription_input(&[])
        .expect_err("0 bytes of audio must be rejected before any network call");
    println!("empty audio -> {err}");

    let err =
        validate_speech_input("").expect_err("empty text must be rejected before any network call");
    println!("empty text -> {err}");

    let err = validate_speech_input("   ")
        .expect_err("whitespace-only text must be rejected before any network call");
    println!("whitespace-only text -> {err}");

    println!(
        "tinfoil_audio_wire_probe: OK -- wire shapes round-trip and empty-input is rejected before any network call."
    );
}
