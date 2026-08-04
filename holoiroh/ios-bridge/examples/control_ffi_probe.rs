//! Executable FFI control-boundary probe.
//!
//! With no arguments it starts strict fake daemons and witnesses greeting
//! verification, bridge-side client signing, caller-bypass rejection, tampered
//! server rejection, bounded oversized-frame handling, and poll error delivery.
//! With `<ticket> <pin>` it exercises the same FFI surface against a real daemon.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use holoiroh_ios_bridge::{
    HOLOIROH_ERR_CONNECT_FAILED, HOLOIROH_OK, holoiroh_ios_bridge_control_connect,
    holoiroh_ios_bridge_control_send, holoiroh_ios_bridge_free,
    holoiroh_ios_bridge_free_error_string, holoiroh_ios_bridge_new,
    holoiroh_ios_bridge_poll_control_event, holoiroh_ios_bridge_ticket_connect,
};
use holoiroh_wire::{
    CONTROL_ALPN, ClientMessage, EnvelopeDirection, ServerMessage, TaskEnvelope,
    decode_ed25519_signature, encode_ed25519_signature, write_line,
};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, PublicKey, SecretKey, Signature};
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const SESSION: &str = "ffi-valid-session";
const MAX_CONTROL_FRAME_BYTES: usize = 96 * 1024 * 1024;

fn blocking<T>(operation: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(operation)
}

fn take_string(pointer: *mut c_char) -> String {
    if pointer.is_null() {
        return "(no detail)".to_string();
    }
    let value = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { holoiroh_ios_bridge_free_error_string(pointer) };
    value
}

fn sign_server(
    envelope: &mut TaskEnvelope<ServerMessage>,
    signing_key: &SecretKey,
    advertised_signer: &PublicKey,
    recipient: &PublicKey,
) {
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::DaemonToClient,
            advertised_signer.as_bytes(),
            recipient.as_bytes(),
        )
        .unwrap();
    envelope.signature = Some(encode_ed25519_signature(
        &signing_key.sign(&payload).to_bytes(),
    ));
}

fn verify_client(
    envelope: &TaskEnvelope<ClientMessage>,
    signer: &PublicKey,
    recipient: &PublicKey,
) {
    assert_eq!(envelope.message_type, envelope.payload.type_tag());
    let bytes = decode_ed25519_signature(
        envelope
            .signature
            .as_deref()
            .expect("bridge forwarded an unsigned envelope"),
    )
    .unwrap();
    let signature = Signature::from_bytes(&bytes);
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            signer.as_bytes(),
            recipient.as_bytes(),
        )
        .unwrap();
    signer.verify(&payload, &signature).unwrap();
}

#[derive(Debug, Clone, Copy)]
enum FakeMode {
    WrongGreetingKey,
    ValidThenTamper,
    ValidThenOversized,
}

#[derive(Debug, Clone)]
struct FakeControl {
    endpoint_key: Arc<SecretKey>,
    mode: FakeMode,
    saw_signed_client: Arc<AtomicBool>,
}

impl ProtocolHandler for FakeControl {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let client_public = connection.remote_id();
        let (mut send, recv) = connection
            .accept_bi()
            .await
            .map_err(AcceptError::from_err)?;
        let mut lines = BufReader::new(recv).lines();
        let pin = lines
            .next_line()
            .await
            .map_err(AcceptError::from_err)?
            .ok_or_else(|| AcceptError::from_err(std::io::Error::other("missing PIN")))?;
        let value: serde_json::Value = serde_json::from_str(&pin).map_err(AcceptError::from_err)?;
        assert_eq!(value["type"], "pin");

        let endpoint_public = self.endpoint_key.public();
        let mut greeting = TaskEnvelope::<ServerMessage>::wrap(
            SESSION.to_string(),
            None,
            0,
            ServerMessage::status("control channel ready"),
        );
        match self.mode {
            FakeMode::WrongGreetingKey => {
                sign_server(
                    &mut greeting,
                    &SecretKey::generate(),
                    &endpoint_public,
                    &client_public,
                );
                write_line(&mut send, &greeting)
                    .await
                    .map_err(AcceptError::from_err)?;
                tokio::time::sleep(Duration::from_millis(500)).await;
                return Ok(());
            }
            FakeMode::ValidThenTamper | FakeMode::ValidThenOversized => {
                sign_server(
                    &mut greeting,
                    &self.endpoint_key,
                    &endpoint_public,
                    &client_public,
                );
                write_line(&mut send, &greeting)
                    .await
                    .map_err(AcceptError::from_err)?;
            }
        }

        if matches!(self.mode, FakeMode::ValidThenOversized) {
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..=MAX_CONTROL_FRAME_BYTES / chunk.len() {
                send.write_all(&chunk)
                    .await
                    .map_err(AcceptError::from_err)?;
            }
            send.write_all(b"\n").await.map_err(AcceptError::from_err)?;
            send.flush().await.map_err(AcceptError::from_err)?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Ok(());
        }

        let line = lines
            .next_line()
            .await
            .map_err(AcceptError::from_err)?
            .ok_or_else(|| {
                AcceptError::from_err(std::io::Error::other("missing client envelope"))
            })?;
        let envelope: TaskEnvelope<ClientMessage> =
            serde_json::from_str(&line).map_err(AcceptError::from_err)?;
        assert_eq!(envelope.session_id, SESSION);
        assert_eq!(envelope.sequence_number, 0);
        verify_client(&envelope, &client_public, &endpoint_public);
        self.saw_signed_client.store(true, Ordering::Release);

        let mut ack = TaskEnvelope::<ServerMessage>::wrap(
            SESSION.to_string(),
            envelope.task_id.clone(),
            1,
            ServerMessage::ack(),
        );
        sign_server(
            &mut ack,
            &self.endpoint_key,
            &endpoint_public,
            &client_public,
        );
        write_line(&mut send, &ack)
            .await
            .map_err(AcceptError::from_err)?;

        let mut tampered = TaskEnvelope::<ServerMessage>::wrap(
            SESSION.to_string(),
            None,
            2,
            ServerMessage::status("signed original"),
        );
        sign_server(
            &mut tampered,
            &self.endpoint_key,
            &endpoint_public,
            &client_public,
        );
        tampered.payload = ServerMessage::status("tampered after signing");
        write_line(&mut send, &tampered)
            .await
            .map_err(AcceptError::from_err)?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }
}

struct FakeDaemon {
    ticket: String,
    router: iroh::protocol::Router,
    saw_signed_client: Arc<AtomicBool>,
}

async fn fake_daemon(mode: FakeMode) -> FakeDaemon {
    let key = Arc::new(SecretKey::generate());
    let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key((*key).clone())
        .bind()
        .await
        .unwrap();
    let saw_signed_client = Arc::new(AtomicBool::new(false));
    let handler = FakeControl {
        endpoint_key: key,
        mode,
        saw_signed_client: saw_signed_client.clone(),
    };
    let router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(CONTROL_ALPN, handler)
        .spawn();
    let ticket = LiveTicket::new(endpoint.addr(), "fake-control").to_string();
    FakeDaemon {
        ticket,
        router,
        saw_signed_client,
    }
}

unsafe fn connect_bridge(
    ticket: &str,
    pin: &str,
) -> (*mut holoiroh_ios_bridge::HoloirohBridge, i32, String) {
    let bridge = unsafe { holoiroh_ios_bridge_new() };
    assert!(!bridge.is_null());
    let ticket = CString::new(ticket).unwrap();
    let mut error = std::ptr::null_mut();
    let _ = unsafe { holoiroh_ios_bridge_ticket_connect(bridge, ticket.as_ptr(), &mut error) };
    if !error.is_null() {
        let _ = take_string(error);
    }
    let pin = CString::new(pin).unwrap();
    let mut error = std::ptr::null_mut();
    let status = unsafe { holoiroh_ios_bridge_control_connect(bridge, pin.as_ptr(), &mut error) };
    (bridge, status, take_string(error))
}

unsafe fn poll_once(
    bridge: *mut holoiroh_ios_bridge::HoloirohBridge,
) -> (i32, Option<String>, String) {
    let mut json = std::ptr::null_mut();
    let mut error = std::ptr::null_mut();
    let status = unsafe { holoiroh_ios_bridge_poll_control_event(bridge, &mut json, &mut error) };
    let event = (!json.is_null()).then(|| take_string(json));
    (status, event, take_string(error))
}

async fn fake_probe() {
    println!("=== wrong transport-key greeting fails control_connect ===");
    let fake = fake_daemon(FakeMode::WrongGreetingKey).await;
    let (bridge, status, error) = blocking(|| unsafe { connect_bridge(&fake.ticket, "123456") });
    assert_eq!(status, HOLOIROH_ERR_CONNECT_FAILED);
    assert!(error.contains("signature"), "unexpected error: {error}");
    blocking(|| unsafe { holoiroh_ios_bridge_free(bridge) });
    fake.router.shutdown().await.unwrap();

    println!("=== unsigned Swift envelope is signed; bypasses are rejected ===");
    let fake = fake_daemon(FakeMode::ValidThenTamper).await;
    let (bridge, status, error) = blocking(|| unsafe { connect_bridge(&fake.ticket, "123456") });
    assert_eq!(status, HOLOIROH_OK, "{error}");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let unsigned = TaskEnvelope::<ClientMessage>::wrap(
        SESSION.to_string(),
        Some("ffi-task".to_string()),
        0,
        ClientMessage::Prompt {
            text: "ffi signed by bridge".to_string(),
        },
    );

    let mut caller_signed = unsigned.clone();
    caller_signed.signature = Some(format!("ed25519:{}", "00".repeat(64)));
    let caller_signed = CString::new(serde_json::to_string(&caller_signed).unwrap()).unwrap();
    let mut error = std::ptr::null_mut();
    let status = blocking(|| unsafe {
        holoiroh_ios_bridge_control_send(bridge, caller_signed.as_ptr(), &mut error)
    });
    assert!(status < 0);
    assert!(take_string(error).contains("caller-supplied"));

    let bare = CString::new(r#"{"type":"prompt","text":"bypass"}"#).unwrap();
    let mut error = std::ptr::null_mut();
    let status =
        blocking(|| unsafe { holoiroh_ios_bridge_control_send(bridge, bare.as_ptr(), &mut error) });
    assert!(status < 0);
    assert!(take_string(error).contains("TaskEnvelope"));

    let unsigned_json = CString::new(serde_json::to_string(&unsigned).unwrap()).unwrap();
    let mut error = std::ptr::null_mut();
    let status = blocking(|| unsafe {
        holoiroh_ios_bridge_control_send(bridge, unsigned_json.as_ptr(), &mut error)
    });
    assert_eq!(status, HOLOIROH_OK, "{}", take_string(error));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !fake.saw_signed_client.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "fake daemon never saw signed client"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, event, error) = unsafe { poll_once(bridge) };
    assert_eq!(status, HOLOIROH_ERR_CONNECT_FAILED);
    assert!(event.is_none(), "tampered event must never be delivered");
    assert!(
        error.contains("signature"),
        "unexpected poll error: {error}"
    );
    blocking(|| unsafe { holoiroh_ios_bridge_free(bridge) });
    fake.router.shutdown().await.unwrap();
    println!("bridge signing and tampered-event rejection OK at {now}");

    println!("=== oversized authenticated frame is bounded and reported ===");
    let fake = fake_daemon(FakeMode::ValidThenOversized).await;
    let (bridge, status, error) = blocking(|| unsafe { connect_bridge(&fake.ticket, "123456") });
    assert_eq!(status, HOLOIROH_OK, "{error}");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, event, error) = unsafe { poll_once(bridge) };
        if status == HOLOIROH_ERR_CONNECT_FAILED {
            assert!(event.is_none());
            assert!(
                error.contains("exceeds"),
                "unexpected oversized error: {error}"
            );
            break;
        }
        assert_eq!(status, HOLOIROH_OK, "{error}");
        assert!(
            std::time::Instant::now() < deadline,
            "oversized frame was not reported"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    blocking(|| unsafe { holoiroh_ios_bridge_free(bridge) });
    fake.router.shutdown().await.unwrap();

    println!("control_ffi_probe: ALL FAKE-DAEMON CHECKS PASSED");
}

async fn real_probe(ticket: String, pin: String) {
    let (bridge, status, error) = blocking(|| unsafe { connect_bridge(&ticket, &pin) });
    assert_eq!(status, HOLOIROH_OK, "control_connect failed: {error}");

    let session_id = loop {
        let (status, event, error) = unsafe { poll_once(bridge) };
        assert_eq!(status, HOLOIROH_OK, "poll greeting failed: {error}");
        if let Some(event) = event {
            let envelope: TaskEnvelope<ServerMessage> = serde_json::from_str(&event).unwrap();
            break envelope.session_id;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let unsigned = TaskEnvelope::<ClientMessage>::wrap(
        session_id,
        Some("control-ffi-real".to_string()),
        0,
        ClientMessage::Prompt {
            text: "control ffi real-daemon probe".to_string(),
        },
    );
    let json = CString::new(serde_json::to_string(&unsigned).unwrap()).unwrap();
    let mut error = std::ptr::null_mut();
    let status =
        blocking(|| unsafe { holoiroh_ios_bridge_control_send(bridge, json.as_ptr(), &mut error) });
    assert_eq!(status, HOLOIROH_OK, "send failed: {}", take_string(error));
    println!("real daemon accepted Swift-style unsigned envelope; bridge signed it");
    blocking(|| unsafe { holoiroh_ios_bridge_free(bridge) });
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut args = std::env::args().skip(1);
    match (args.next(), args.next()) {
        (Some(ticket), Some(pin)) => real_probe(ticket, pin).await,
        (None, None) => fake_probe().await,
        _ => panic!("usage: control_ffi_probe [<iroh-live:ticket> <pin>]"),
    }
}
