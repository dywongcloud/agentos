//! Provides an `extern "C"` foreign function interface (FFI) between `iroh-live` and the iOS Swift app.
//!
//! # Purpose
//!
//! Neither `iroh` nor `iroh-live` provides official Swift bindings for the `iroh-live` media API.
//! The separate `n0-computer/iroh-ffi` repository provides official Swift bindings for base `iroh`.
//! Those bindings wrap `Endpoint` and `Connection`.
//! They do not wrap `LocalBroadcast`, `subscribe`, or frame polling.
//! See `../ios/IROH_FFI.md` for sources and the decision rationale.
//!
//! This crate provides a minimal hand-written bridge.
//! The upstream `moq-media-android` crate uses the same hand-written bridge pattern for Kotlin and Java Native Interface (JNI).
//!
//! # Observable behavior
//!
//! The implementation uses the `iroh-live` subscribe API from commit `5f95758`.
//! `Cargo.toml` pins that commit.
//!
//! - Connect: [`iroh::Endpoint::builder`]`(presets::N0).bind()` then [`iroh_live::Live::builder`]`(ep).with_router().spawn()`.
//! - Ticket: [`iroh_live::ticket::LiveTicket::from_str`] returns public `endpoint: EndpointAddr` and `broadcast_name: String` fields.
//! - Subscribe: [`iroh_live::Live::subscribe`]`(ticket.endpoint, &ticket.broadcast_name)` returns [`iroh_live::Subscription`].
//! - Video: `subscription.broadcast().video_ready()` waits for a catalog video rendition.
//!   It subscribes to the best-quality rendition.
//!   Apple targets use VideoToolbox as the default decoder.
//! - Frames: `VideoTrack::try_recv()` returns `Option<moq_media::format::VideoFrame>` without blocking.
//!   `frame.rgba_image().as_raw()` normalizes packed RGBA, packed BGRA, GPU, or NV12 input.
//!   The bridge converts this data to tightly packed BGRA8 output.
//!   Each row contains `width * 4` bytes.
//!   The complete buffer contains `width * height * 4` bytes.
//!   Swift maps this output to a `kCVPixelFormatType_32BGRA` `CVPixelBuffer`.
//!
//! The control channel uses the separate `holoiroh/control/1` application-layer protocol negotiation (ALPN) identifier.
//! Both sides share [`holoiroh_wire::CONTROL_ALPN`].
//! The control connection dials the ticket peer with `live.endpoint().connect(peer, CONTROL_ALPN)`.
//! It opens one bidirectional stream and performs the daemon's bare-line personal identification number (PIN) handshake.
//! A reader task then sends received newline-delimited JSON (NDJSON) lines to an event queue.
//! Each control function documents its wire contract.
//!
//! # FFI invariants
//!
//! - Stateful objects cross the boundary as opaque pointers to zero-sized marker types.
//!   A `_new` or `_subscribe` function creates each handle.
//!   The matching `_free` function releases it.
//!   Distinct marker types prevent callers from exchanging bridge and subscription handles.
//! - C application binary interfaces (ABIs) do not support `async fn`.
//!   Each `extern "C"` function is synchronous for its caller.
//!   `BridgeInner` owns a multithreaded Tokio runtime.
//!   Connect and subscribe calls use `runtime.block_on(...)`.
//!   Polling calls return immediately after a non-blocking `try_recv`.
//!   Swift must run connection, subscription, and polling work outside the main thread.
//! - A panic must not unwind across the FFI boundary.
//!   Rust defines such unwinding as undefined behavior.
//!   Each fallible function uses [`std::panic::catch_unwind`].
//!   Failure returns a null pointer or negative integer.
//!   An optional `out_error: *mut *mut c_char` points to a writable `*mut c_char` slot.
//!   On failure, the function can store a heap-allocated, null-terminated string in that slot.
//!   The caller must use [`holoiroh_ios_bridge_free_error_string`] to free that string.
//! - [`holoiroh_ios_bridge_poll_next_frame`] writes frames into caller-owned buffers.
//!   This design avoids one Rust allocation for each frame at 30-60 frames per second.
//!   If capacity is insufficient, the function returns [`HOLOIROH_ERR_BUFFER_TOO_SMALL`].
//!   It writes the actual dimensions to `out_frame`.
//!   The caller can allocate `width * height * 4` bytes before polling again.
//!
//! # iOS `.xcframework` packaging
//!
//! See the witnessed commands in `../ios/IROH_FFI.md` under "As-built: xcframework packaging."
//!
//! 1. Run `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios-sim`.
//! 2. Run `cargo build -p holoiroh-ios-bridge --target <triple> --release` for each target.
//!    Each archive is `target/<triple>/release/libholoiroh_ios_bridge.a`.
//! 3. Use `lipo -create` to combine both simulator slices into one fat `.a` archive.
//! 4. Generate the committed `include/HoloirohIosBridge.h` and adjacent `module.modulemap` files.
//! 5. Use `xcodebuild -create-xcframework` with the device slice and combined simulator slice.
//!    Pair each slice with `-headers include/`.
//! 6. Add the `.xcframework` to the Xcode or Swift Package Manager target.
//!    Use `import HoloirohIosBridge` from Swift.
//!    `../ios/Sources/HoloIrohApp/Video/IrohLiveFrameSource.swift` wraps the C functions as a `VideoFrameSource`.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use holoiroh_wire::{
    ClientMessage, EnvelopeDirection, InboundEnvelopeState, PROTOCOL_VERSION, ServerMessage,
    TaskEnvelope, decode_ed25519_signature, encode_ed25519_signature,
};
use iroh::endpoint::{Connection, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, PublicKey, SecretKey, Signature};
use iroh_live::Live;
use iroh_live::media::subscribe::VideoTrack;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::runtime::Runtime;

/// Stores an opaque bridge handle.
/// [`holoiroh_ios_bridge_new`] creates the handle.
/// [`holoiroh_ios_bridge_free`] releases it.
///
/// Rust never constructs this zero-sized marker as bridge state.
/// The implementation casts a boxed [`BridgeInner`] pointer to this type.
/// C and Swift can hold the pointer but cannot access the private Rust layout.
/// Distinct bridge and subscription marker types prevent callers from exchanging their handles.
#[repr(C)]
pub struct HoloirohBridge {
    _private: [u8; 0],
}

/// Stores an opaque active-video-subscription handle.
/// A bridge can maintain zero or one active subscription independently of its connection lifecycle.
/// [`holoiroh_ios_bridge_subscribe`] creates this handle.
/// [`holoiroh_ios_bridge_subscription_free`] releases it.
#[repr(C)]
pub struct HoloirohSubscription {
    _private: [u8; 0],
}

/// Contains metadata for one decoded media frame.
/// [`holoiroh_ios_bridge_poll_next_frame`] fills this structure.
/// That function writes frame bytes to a separate caller-owned buffer.
/// Swift uses this metadata to configure `CVPixelBuffer` or `AVSampleBufferDisplayLayer`.
#[repr(C)]
pub struct HoloirohFrame {
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp in microseconds since the subscription started.
    /// This value comes from the decoded frame's `timestamp`.
    /// It is `Duration::ZERO` before the pipeline assigns a presentation timestamp (PTS).
    pub timestamp_us: u64,
    /// Identifies the pixel format in the caller's buffer.
    /// This build always writes [`HOLOIROH_PIXFMT_BGRA8`].
    /// `VideoFrame::rgba_image()` first produces tightly packed 8-bit RGBA data.
    /// The bridge swaps the red and blue bytes during copy-out.
    /// Each row contains `width * 4` bytes.
    /// The complete buffer contains `width * height * 4` bytes.
    /// Swift uses this field to select the matching `kCVPixelFormatType_*`.
    /// A future zero-copy path can report another format without an application binary interface change.
    pub pixel_format: u32,
    /// `0` identifies video.
    /// `1` identifies audio.
    /// A plain integer tag crosses the foreign function interface (FFI) safely.
    /// A `#[repr(C)]` Rust `enum` is more difficult to bind from Swift.
    /// This build always writes `0`.
    /// This bridge does not currently provide audio.
    pub kind: u8,
}

/// Result/error-code convention shared by every fallible function in this
/// module: `0` = success, negative = failure. The sign convention is fixed:
/// callers distinguish success from failure by the sign of the return value.
pub type HoloirohStatus = c_int;

pub const HOLOIROH_OK: HoloirohStatus = 0;
pub const HOLOIROH_ERR_UNKNOWN: HoloirohStatus = -1;
pub const HOLOIROH_ERR_INVALID_TICKET: HoloirohStatus = -2;
pub const HOLOIROH_ERR_CONNECT_FAILED: HoloirohStatus = -3;
pub const HOLOIROH_ERR_NOT_CONNECTED: HoloirohStatus = -4;
pub const HOLOIROH_ERR_BUFFER_TOO_SMALL: HoloirohStatus = -5;
/// The subscription's video track has ended (producer dropped): no further
/// frames will ever arrive. Distinct from "no frame yet" (`0`).
pub const HOLOIROH_ERR_ENDED: HoloirohStatus = -6;
/// This build does not support the requested operation.
/// No function currently returns this status.
/// Earlier control-channel functions returned it before their implementation.
/// The constant preserves the C application binary interface (ABI) error-code numbering.
pub const HOLOIROH_ERR_UNSUPPORTED: HoloirohStatus = -7;
/// A required pointer argument was null.
pub const HOLOIROH_ERR_NULL_ARG: HoloirohStatus = -8;
/// The bridge caught a Rust panic at the foreign function interface (FFI) boundary.
/// The boundary returns this status instead of unwinding into C.
/// Unwinding into C causes undefined behavior.
pub const HOLOIROH_ERR_PANIC: HoloirohStatus = -9;

/// Pixel-format tag for [`HoloirohFrame::pixel_format`]: tightly-packed 8-bit
/// RGBA (R,G,B,A byte order), `width * 4` bytes per row. Historical: no
/// longer emitted by this build -- see [`HOLOIROH_PIXFMT_BGRA8`].
pub const HOLOIROH_PIXFMT_RGBA8: u32 = 0;

/// Tags tightly packed 8-bit BGRA data in [`HoloirohFrame::pixel_format`].
/// Byte order is B, G, R, A.
/// Each row contains `width * 4` bytes.
/// [`holoiroh_ios_bridge_poll_next_frame`] emits this format.
/// Swift maps it to `kCVPixelFormatType_32BGRA`.
///
/// iOS does not support `kCVPixelFormatType_32RGBA` for Core Video pools or IOSurface.
/// A live device probe showed that `CVPixelBufferPoolCreate` returned nil for that format.
/// The display therefore dropped every frame while decoding continued at 20-40 frames per second.
/// The device console showed `vdec stats fps=20..40` and no errors.
/// The bridge swaps red and blue bytes during copy-out because iOS supports 32BGRA.
pub const HOLOIROH_PIXFMT_BGRA8: u32 = 1;

/// Re-exports the control channel application-layer protocol negotiation (ALPN) identifier.
/// Both the bridge and daemon import [`holoiroh_wire::CONTROL_ALPN`].
/// This shared definition prevents byte-string drift.
/// See `holoiroh-wire/src/lib.rs` for the wire schema.
/// The public re-export preserves existing unqualified `CONTROL_ALPN` references.
pub use holoiroh_wire::CONTROL_ALPN;

/// Limits how long playout waits for an incomplete group before skipping it.
///
/// This value controls head-of-line tolerance, not a fixed delay.
/// A clean link adds no delay.
/// Loss or reordering can stall playout for at most 60 milliseconds.
/// The library default is 150 milliseconds for recorded media.
/// The adjacent `Sync` jitter buffer uses 20 milliseconds for the remote-control feedback loop.
///
/// `video_latency_probe` measured 66-172 milliseconds to resume at the next group boundary.
/// Its median result was 80 milliseconds.
/// The 60-millisecond limit remains below that median.
/// It also covers approximately two frames at the measured rate of approximately 28 frames per second.
///
/// The encoder uses a one-second group of pictures (GOP).
/// Skipping does not require waiting for the next independent decoder refresh (IDR) frame.
/// The probe showed that each group starts with a keyframe.
const PLAYOUT_MAX_LATENCY: std::time::Duration = std::time::Duration::from_millis(60);

/// Waits for the daemon's first reply after writing the personal identification number (PIN) line.
/// The reply is either bare `auth_rejected` or an envelope-wrapped ready greeting.
/// The 30-second limit permits a relay round trip and the daemon's first-pairing allowlist save.
/// The QUIC connection has already succeeded when this timer starts.
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONTROL_FRAME_BYTES: usize = 96 * 1024 * 1024;

async fn read_bounded_control_line<R>(reader: &mut R) -> Result<Option<String>, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(MAX_CONTROL_FRAME_BYTES.min(8 * 1024));
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| format!("control-channel read failed: {error}"))?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        if content_len > MAX_CONTROL_FRAME_BYTES.saturating_sub(bytes.len()) {
            return Err(format!(
                "control frame exceeds {MAX_CONTROL_FRAME_BYTES}-byte limit"
            ));
        }
        bytes.extend_from_slice(&available[..content_len]);
        reader.consume(newline.map_or(content_len, |index| index + 1));
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "control frame is not valid UTF-8".to_string())
}

// ---------------------------------------------------------------------
// Private Rust state behind the opaque handles
// ---------------------------------------------------------------------

/// The real state behind a [`HoloirohBridge`] opaque pointer. Never
/// `#[repr(C)]`; only ever reached via `&*(ptr as *const BridgeInner)`.
struct BridgeInner {
    /// This bridge owns a multithreaded Tokio runtime.
    /// All asynchronous `iroh-live` calls use `block_on` on this runtime.
    /// Rust drops bridge fields in declaration order.
    runtime: Runtime,
    /// The `iroh-live` session (owns the iroh `Endpoint` + MoQ transport).
    live: Live,
    /// The active subscription, once [`holoiroh_ios_bridge_ticket_connect`]
    /// has succeeded. Behind a `Mutex` because the connect call and any later
    /// access can come from different Swift threads.
    subscription: Mutex<Option<iroh_live::Subscription>>,
    /// The daemon peer address parsed out of the last successful
    /// [`holoiroh_ios_bridge_ticket_connect`] ticket (`LiveTicket::endpoint`)
    /// -- the address [`holoiroh_ios_bridge_control_connect`] dials on
    /// [`CONTROL_ALPN`]. `None` until a ticket has been parsed.
    control_peer: Mutex<Option<EndpointAddr>>,
    /// Stores the active control channel after [`holoiroh_ios_bridge_control_connect`] succeeds.
    /// One lock provides connection idempotency and serialized writes.
    /// The connection call holds the lock throughout dialing.
    /// Concurrent connection calls therefore serialize instead of dialing twice.
    /// [`holoiroh_ios_bridge_control_send`] also serializes writes through this lock.
    control: Mutex<Option<ControlState>>,
    /// NDJSON lines read off the control stream by the reader task spawned
    /// in [`holoiroh_ios_bridge_control_connect`], drained one line per
    /// [`holoiroh_ios_bridge_poll_control_event`] call. `Arc` because the
    /// reader task (spawned on `runtime`, hence `'static`) shares it with
    /// the FFI side.
    control_events: Arc<Mutex<VecDeque<String>>>,
    /// Bounded fatal reader error reported by the polling ABI before any
    /// queued event can be delivered.
    control_error: Arc<Mutex<Option<String>>>,
    /// Monotonic reconnect generation. Reader tasks may mutate shared state
    /// only while this still equals the generation they were spawned with.
    control_generation: Arc<AtomicU64>,
    /// Set by the reader task on stream end (EOF or read error) and by a
    /// failed [`holoiroh_ios_bridge_control_send`]. Once the event queue is
    /// also drained, [`holoiroh_ios_bridge_poll_control_event`] reports
    /// [`HOLOIROH_ERR_ENDED`].
    control_ended: Arc<AtomicBool>,
}

/// Stores the active control-channel transport in [`BridgeInner::control`].
/// The stored [`Connection`] keeps the iroh QUIC connection active with its send half.
/// The receive half remains in the reader task.
/// Iroh closes the connection after every handle is dropped.
/// [`holoiroh_ios_bridge_free`] uses this stored handle to close the connection explicitly.
struct ControlState {
    connection: Connection,
    send: SendStream,
    session_id: String,
    daemon_public: PublicKey,
    next_outbound_sequence: u64,
}

/// Stores the decoded video track behind a [`HoloirohSubscription`] handle.
/// `try_recv` and `next_frame` require mutable access.
/// A background Swift thread can call the poll function.
/// The `Mutex` therefore protects the track.
struct SubscriptionInner {
    track: Mutex<VideoTrack>,
}

// ---------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------

/// Write `msg` as a freshly-allocated null-terminated C string into
/// `*out_error`, if `out_error` is non-null. The caller must free it via
/// [`holoiroh_ios_bridge_free_error_string`]. A NUL byte inside `msg` is
/// handled by substituting a fixed fallback (via `CString::new`'s error path),
/// never by panicking.
///
/// # Safety
/// `out_error`, if non-null, must be a valid writable `*mut *mut c_char`.
unsafe fn set_error(out_error: *mut *mut c_char, msg: &str) {
    if out_error.is_null() {
        return;
    }
    let cstring = CString::new(msg)
        .unwrap_or_else(|_| CString::new("error message contained a NUL byte").unwrap());
    unsafe {
        *out_error = cstring.into_raw();
    }
}

/// Borrow a `BridgeInner` from an opaque `*mut HoloirohBridge`, or `None` if
/// null.
///
/// # Safety
/// `bridge` must be null or a live pointer from [`holoiroh_ios_bridge_new`].
unsafe fn bridge_ref<'a>(bridge: *mut HoloirohBridge) -> Option<&'a BridgeInner> {
    if bridge.is_null() {
        return None;
    }
    Some(unsafe { &*(bridge as *const BridgeInner) })
}

const MAX_CONTROL_ERROR_BYTES: usize = 256;

fn bounded_control_error(message: impl AsRef<str>) -> String {
    let message = message.as_ref();
    if message.len() <= MAX_CONTROL_ERROR_BYTES {
        return message.to_string();
    }
    let mut end = MAX_CONTROL_ERROR_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_string()
}

fn verify_daemon_envelope(
    envelope: &TaskEnvelope<serde_json::Value>,
    signer: &PublicKey,
    recipient: &PublicKey,
) -> Result<(), String> {
    let encoded = envelope
        .signature
        .as_deref()
        .ok_or_else(|| "daemon envelope signature is required".to_string())?;
    let bytes = decode_ed25519_signature(encoded)
        .map_err(|error| format!("invalid daemon signature encoding: {error}"))?;
    let signature = Signature::from_bytes(&bytes);
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::DaemonToClient,
            signer.as_bytes(),
            recipient.as_bytes(),
        )
        .map_err(|error| format!("invalid daemon signing payload: {error}"))?;
    signer
        .verify(&payload, &signature)
        .map_err(|_| "daemon envelope signature verification failed".to_string())
}

fn sign_client_envelope(
    envelope: &mut TaskEnvelope<ClientMessage>,
    signer: &SecretKey,
    recipient: &PublicKey,
) -> Result<(), String> {
    let signer_public = signer.public();
    let payload = envelope
        .signing_payload(
            EnvelopeDirection::ClientToDaemon,
            signer_public.as_bytes(),
            recipient.as_bytes(),
        )
        .map_err(|error| format!("invalid client signing payload: {error}"))?;
    let signature = signer.sign(&payload);
    envelope.signature = Some(encode_ed25519_signature(&signature.to_bytes()));
    Ok(())
}

fn validate_server_line(
    line: &str,
    daemon_public: &PublicKey,
    local_public: &PublicKey,
    inbound: &mut InboundEnvelopeState,
) -> Result<TaskEnvelope<ServerMessage>, String> {
    let envelope: TaskEnvelope<serde_json::Value> =
        serde_json::from_str(line).map_err(|error| format!("invalid daemon envelope: {error}"))?;
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err("unsupported daemon envelope protocol_version".to_string());
    }
    verify_daemon_envelope(&envelope, daemon_public, local_public)?;
    let payload: ServerMessage = serde_json::from_value(envelope.payload.clone())
        .map_err(|error| format!("invalid daemon payload: {error}"))?;
    if envelope.message_type != payload.type_tag() {
        return Err("daemon envelope message_type mismatch".to_string());
    }
    inbound
        .validate_inbound(&envelope)
        .map_err(|error| format!("daemon envelope state rejected: {error}"))?;
    Ok(TaskEnvelope {
        protocol_version: envelope.protocol_version,
        message_id: envelope.message_id,
        session_id: envelope.session_id,
        task_id: envelope.task_id,
        message_type: envelope.message_type,
        sent_at: envelope.sent_at,
        expires_at: envelope.expires_at,
        sequence_number: envelope.sequence_number,
        payload,
        signature: envelope.signature,
    })
}

// ---------------------------------------------------------------------
// Lifecycle: bridge construction / teardown
// ---------------------------------------------------------------------

const IROH_SECRET_KEY_LEN: usize = 32;

unsafe fn secret_key_from_raw(key_ptr: *const u8, key_len: usize) -> Option<SecretKey> {
    if key_ptr.is_null() || key_len != IROH_SECRET_KEY_LEN {
        return None;
    }
    let mut key_bytes = [0u8; IROH_SECRET_KEY_LEN];
    unsafe {
        std::ptr::copy_nonoverlapping(key_ptr, key_bytes.as_mut_ptr(), IROH_SECRET_KEY_LEN);
    }
    Some(SecretKey::from_bytes(&key_bytes))
}

fn initialize_tracing() {
    static TRACING_INIT: std::sync::Once = std::sync::Once::new();
    TRACING_INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "warn,moq_media=debug,rusty_codecs=debug,moq_net=info,iroh_moq=info",
            ))
            .with_writer(std::io::stderr)
            .try_init();
    });
}

fn create_bridge(secret_key: Option<SecretKey>) -> *mut HoloirohBridge {
    initialize_tracing();
    let result = catch_unwind(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .ok()?;
        let live = runtime.block_on(async {
            let builder = Endpoint::builder(presets::N0)
                .address_lookup(iroh_mdns_address_lookup::MdnsAddressLookup::builder());
            let builder = match secret_key {
                Some(secret_key) => builder.secret_key(secret_key),
                None => builder,
            };
            let endpoint = builder.bind().await.ok()?;
            Some(Live::builder(endpoint).with_router().spawn())
        })?;
        let inner = Box::new(BridgeInner {
            runtime,
            live,
            subscription: Mutex::new(None),
            control_peer: Mutex::new(None),
            control: Mutex::new(None),
            control_events: Arc::new(Mutex::new(VecDeque::new())),
            control_error: Arc::new(Mutex::new(None)),
            control_generation: Arc::new(AtomicU64::new(0)),
            control_ended: Arc::new(AtomicBool::new(false)),
        });
        Some(Box::into_raw(inner) as *mut HoloirohBridge)
    });
    match result {
        Ok(Some(ptr)) => ptr,
        Ok(None) | Err(_) => std::ptr::null_mut(),
    }
}

/// Creates a bridge with a generated, process-lifetime iroh identity.
///
/// This constructor remains available for ABI compatibility. Persistent clients
/// must use [`holoiroh_ios_bridge_new_with_secret_key`].
///
/// # Safety
/// The returned pointer, if non-null, must be released exactly once with
/// [`holoiroh_ios_bridge_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_new() -> *mut HoloirohBridge {
    create_bridge(None)
}

/// Creates a bridge with an endpoint identity derived from a 32-byte secret-key seed.
/// `key_ptr` must be non-null.
/// `key_len` must equal `32`.
/// Invalid input or runtime construction failure returns null.
/// Endpoint construction failure also returns null.
///
/// # Safety
///
/// `key_ptr` must be readable for exactly `key_len` bytes.
/// The caller must release a non-null result exactly once with [`holoiroh_ios_bridge_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_new_with_secret_key(
    key_ptr: *const u8,
    key_len: usize,
) -> *mut HoloirohBridge {
    let Some(secret_key) = (unsafe { secret_key_from_raw(key_ptr, key_len) }) else {
        return std::ptr::null_mut();
    };
    create_bridge(Some(secret_key))
}

/// Releases a bridge and shuts down its active resources.
/// This function drops any active subscription.
/// It calls `live.shutdown().await` for the `iroh_live::Live` session.
/// It then drops bridge fields in declaration order.
/// A null pointer has no effect, consistent with `free(NULL)`.
///
/// # Safety
///
/// `bridge` must be null or an unfreed pointer returned by a bridge constructor.
/// The caller must not use `bridge` after this call.
/// The caller must first free every [`HoloirohSubscription`] obtained from this bridge.
/// The bridge runtime drives each subscription's video track.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_free(bridge: *mut HoloirohBridge) {
    if bridge.is_null() {
        return;
    }
    // Reconstruct the Box and run graceful async shutdown before the runtime
    // itself is dropped. Wrapped in catch_unwind so a shutdown panic can never
    // unwind across the FFI boundary.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let inner: Box<BridgeInner> = unsafe { Box::from_raw(bridge as *mut BridgeInner) };
        // Drop any subscription first, then shut the Live session down on the
        // runtime. The Box's own drop then releases `live` and finally the
        // `runtime` (declared last in BridgeInner).
        {
            let mut sub = inner.subscription.lock().unwrap_or_else(|e| e.into_inner());
            *sub = None;
        }
        // Close the control channel (if connected) before shutting the Live
        // session down; the reader task ends on its own when the closed
        // stream EOFs, and is torn down with the runtime regardless.
        {
            inner.control_generation.fetch_add(1, Ordering::AcqRel);
            let mut control = inner.control.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = control.take() {
                state.connection.close(0u32.into(), b"bridge freed");
            }
        }
        inner.runtime.block_on(async {
            inner.live.shutdown().await;
        });
        // `inner` (and thus its runtime) drops here.
    }));
}

// ---------------------------------------------------------------------
// Reachability probe
// ---------------------------------------------------------------------

unsafe fn probe_reachable(
    ticket_cstr: *const c_char,
    timeout_ms: u64,
    secret_key: Option<SecretKey>,
) -> bool {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if ticket_cstr.is_null() {
            return false;
        }
        let ticket_str = match unsafe { CStr::from_ptr(ticket_cstr) }.to_str() {
            Ok(value) => value,
            Err(_) => return false,
        };
        let ticket = match LiveTicket::from_str(ticket_str) {
            Ok(value) => value,
            Err(_) => return false,
        };
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(value) => value,
            Err(_) => return false,
        };
        runtime.block_on(async move {
            let builder = Endpoint::builder(presets::N0)
                .address_lookup(iroh_mdns_address_lookup::MdnsAddressLookup::builder());
            let builder = match secret_key {
                Some(secret_key) => builder.secret_key(secret_key),
                None => builder,
            };
            let endpoint = match builder.bind().await {
                Ok(endpoint) => endpoint,
                Err(_) => return false,
            };
            let peer = ticket.endpoint.clone();
            let dial = async {
                let connection = endpoint.connect(peer, CONTROL_ALPN).await.map_err(|_| ())?;
                connection.open_bi().await.map_err(|_| ())?;
                Ok::<(), ()>(())
            };
            let reachable = matches!(
                tokio::time::timeout(Duration::from_millis(timeout_ms.max(500)), dial).await,
                Ok(Ok(()))
            );
            endpoint.close().await;
            reachable
        })
    }));
    result.unwrap_or(false)
}

/// Probes daemon reachability with a temporary generated endpoint identity.
///
/// This function remains available for ABI compatibility. Persistent clients
/// must use [`holoiroh_ios_bridge_probe_reachable_with_secret_key`].
///
/// # Safety
/// `ticket_cstr` must be a valid null-terminated C string, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_probe_reachable(
    ticket_cstr: *const c_char,
    timeout_ms: u64,
) -> bool {
    unsafe { probe_reachable(ticket_cstr, timeout_ms, None) }
}

/// Probes daemon reachability from an endpoint derived from one 32-byte secret
/// key seed. Returns false unless the ticket is valid, `key_ptr` is non-null,
/// and `key_len` is exactly 32. The temporary endpoint is explicitly closed
/// before this function returns.
///
/// # Safety
/// `ticket_cstr` must be a valid null-terminated C string, or null. `key_ptr`
/// must be readable for exactly `key_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_probe_reachable_with_secret_key(
    ticket_cstr: *const c_char,
    timeout_ms: u64,
    key_ptr: *const u8,
    key_len: usize,
) -> bool {
    let Some(secret_key) = (unsafe { secret_key_from_raw(key_ptr, key_len) }) else {
        return false;
    };
    unsafe { probe_reachable(ticket_cstr, timeout_ms, Some(secret_key)) }
}

// ---------------------------------------------------------------------
// Ticket-connect
// ---------------------------------------------------------------------

/// Connects the `iroh_live::Live` session to the peer in an `iroh-live:` ticket.
/// See Finding (b) in `../ios/IROH_FFI.md` for the serialized [`LiveTicket`](iroh_live::ticket::LiveTicket) format.
/// The function subscribes to the ticket's named broadcast.
/// It blocks until the connection succeeds or fails.
/// Call it from a background queue.
///
/// `ticket_cstr` must contain null-terminated UTF-8, such as output from Swift's `String.withCString`.
/// `out_error` can be null when the caller does not need an error message.
/// The [`HoloirohStatus`] result still identifies the failure class.
/// Success returns [`HOLOIROH_OK`].
/// Failure returns a negative [`HoloirohStatus`].
///
/// # Safety
///
/// `bridge` must be a live pointer returned by a bridge constructor.
/// `ticket_cstr` must remain a valid, null-terminated C string during this call.
/// If non-null, `out_error` must point to a writable `*mut c_char` slot.
/// Therefore, `out_error` has type `*mut *mut c_char`.
/// The caller must free any stored string with [`holoiroh_ios_bridge_free_error_string`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_ticket_connect(
    bridge: *mut HoloirohBridge,
    ticket_cstr: *const c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(inner) = (unsafe { bridge_ref(bridge) }) else {
            unsafe { set_error(out_error, "bridge pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        };
        if ticket_cstr.is_null() {
            unsafe { set_error(out_error, "ticket string pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        }

        // 1. Parse the C string into a &str.
        let ticket_str = match unsafe { CStr::from_ptr(ticket_cstr) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                unsafe { set_error(out_error, "ticket string is not valid UTF-8") };
                return HOLOIROH_ERR_INVALID_TICKET;
            }
        };

        // 2. Parse the iroh-live: ticket URI.
        let ticket = match LiveTicket::from_str(ticket_str) {
            Ok(t) => t,
            Err(err) => {
                unsafe { set_error(out_error, &format!("invalid iroh-live ticket: {err}")) };
                return HOLOIROH_ERR_INVALID_TICKET;
            }
        };

        // Record the daemon's dialable address for the control channel:
        // holoiroh_ios_bridge_control_connect dials this same peer on
        // CONTROL_ALPN. Stored as soon as the ticket parses (not only on
        // subscribe success) so a media-side failure -- e.g. the Mac isn't
        // broadcasting yet -- doesn't also block the control channel.
        {
            let mut peer = inner.control_peer.lock().unwrap_or_else(|e| e.into_inner());
            *peer = Some(ticket.endpoint.clone());
        }

        // 3. Connect + subscribe on the runtime. This dials the publisher
        //    (direct P2P with NAT hole-punch, relay fallback) and subscribes to
        //    the named broadcast.
        let subscribe_result = inner.runtime.block_on(async {
            inner
                .live
                .subscribe_with_playback_policy(
                    ticket.endpoint.clone(),
                    &ticket.broadcast_name,
                    iroh_live::media::playout::PlaybackPolicy::default()
                        .with_max_latency(PLAYOUT_MAX_LATENCY),
                )
                .await
        });

        match subscribe_result {
            Ok(subscription) => {
                let mut slot = inner.subscription.lock().unwrap_or_else(|e| e.into_inner());
                *slot = Some(subscription);
                HOLOIROH_OK
            }
            Err(err) => {
                unsafe { set_error(out_error, &format!("subscribe failed: {err}")) };
                HOLOIROH_ERR_CONNECT_FAILED
            }
        }
    }));

    match result {
        Ok(status) => status,
        Err(_) => {
            unsafe { set_error(out_error, "internal panic during ticket_connect") };
            HOLOIROH_ERR_PANIC
        }
    }
}

// ---------------------------------------------------------------------
// Subscribe (video track)
// ---------------------------------------------------------------------

/// Starts the connected broadcast's video decoder pipeline.
/// The function calls `subscription.broadcast().video_ready().await`.
/// It waits until the catalog advertises at least one video rendition.
/// It then subscribes to the best-quality rendition.
/// Apple targets use VideoToolbox for decoding.
/// The function blocks while waiting for a rendition.
/// Call it from a background queue.
/// A disconnected bridge or unavailable video track returns null.
/// If `out_error` is non-null, the function stores an error string through it.
///
/// # Safety
///
/// `bridge` must be a live, connected pointer after [`holoiroh_ios_bridge_ticket_connect`] succeeds.
/// `out_error` uses the exact `*mut *mut c_char` contract documented by that function.
/// The caller must pass each non-null result to [`holoiroh_ios_bridge_subscription_free`] exactly once.
/// The caller must free the subscription before freeing its bridge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_subscribe(
    bridge: *mut HoloirohBridge,
    out_error: *mut *mut c_char,
) -> *mut HoloirohSubscription {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(inner) = (unsafe { bridge_ref(bridge) }) else {
            unsafe { set_error(out_error, "bridge pointer is null") };
            return std::ptr::null_mut();
        };

        // Hold the subscription lock only long enough to run video_ready() on
        // the runtime.
        let track_result = {
            let slot = inner.subscription.lock().unwrap_or_else(|e| e.into_inner());
            let Some(subscription) = slot.as_ref() else {
                unsafe {
                    set_error(
                        out_error,
                        "bridge is not connected: call holoiroh_ios_bridge_ticket_connect first",
                    )
                };
                return std::ptr::null_mut();
            };
            inner
                .runtime
                .block_on(async { subscription.broadcast().video_ready().await })
        };

        match track_result {
            Ok(track) => {
                let sub_inner = Box::new(SubscriptionInner {
                    track: Mutex::new(track),
                });
                Box::into_raw(sub_inner) as *mut HoloirohSubscription
            }
            Err(err) => {
                unsafe { set_error(out_error, &format!("no video track available: {err}")) };
                std::ptr::null_mut()
            }
        }
    }));

    match result {
        Ok(ptr) => ptr,
        Err(_) => {
            unsafe { set_error(out_error, "internal panic during subscribe") };
            std::ptr::null_mut()
        }
    }
}

/// Releases a subscription created by [`holoiroh_ios_bridge_subscribe`].
/// Passing null is a no-op. Dropping the video track stops the decoder
/// pipeline for that track.
///
/// # Safety
/// `subscription` must either be null or a pointer previously returned by
/// [`holoiroh_ios_bridge_subscribe`] and not already freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_subscription_free(
    subscription: *mut HoloirohSubscription,
) {
    if subscription.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _inner: Box<SubscriptionInner> =
            unsafe { Box::from_raw(subscription as *mut SubscriptionInner) };
        // `_inner` (and the VideoTrack inside it) drops here.
    }));
}

// ---------------------------------------------------------------------
// Poll next frame
// ---------------------------------------------------------------------

/// Polls without blocking for the latest decoded video frame.
/// On success, this function fills `out_frame` and writes BGRA8 bytes to caller-owned `buf`.
/// It returns the number of bytes written.
///
/// Return behavior:
///
/// - If no frame is available, it returns `0` and does not modify `out_frame`.
///   Poll again after a short interval on a background queue.
///   `VideoTrack::try_recv()` drains older buffered frames and returns the latest frame.
/// - If a frame fits, it writes `width * height * 4` BGRA8 bytes.
///   It fills `out_frame` and returns that byte count.
/// - If `buf_capacity` is insufficient, it returns [`HOLOIROH_ERR_BUFFER_TOO_SMALL`].
///   It writes the actual `width`, `height`, and `pixel_format` to `out_frame`.
///   The function consumes that frame.
///   Allocate for the largest expected resolution to prevent this result.
/// - If the publisher ends the track, it returns [`HOLOIROH_ERR_ENDED`].
///   No more frames will arrive.
///
/// # Safety
///
/// `subscription` must be a live pointer from [`holoiroh_ios_bridge_subscribe`].
/// `buf` must be writable for `buf_capacity` bytes.
/// `buf` can be null only when `buf_capacity` is `0`.
/// `out_frame` must be a valid, writable `*mut HoloirohFrame`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_poll_next_frame(
    subscription: *mut HoloirohSubscription,
    buf: *mut u8,
    buf_capacity: usize,
    out_frame: *mut HoloirohFrame,
) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if subscription.is_null() || out_frame.is_null() {
            return HOLOIROH_ERR_NULL_ARG;
        }
        let inner = unsafe { &*(subscription as *const SubscriptionInner) };
        let mut track = inner.track.lock().unwrap_or_else(|e| e.into_inner());

        // Non-blocking: take the latest decoded frame (draining older ones).
        match track.try_recv() {
            None => {
                if track.is_closed() {
                    HOLOIROH_ERR_ENDED
                } else {
                    // No frame yet -- poll again later.
                    0
                }
            }
            Some(frame) => {
                let width = frame.width();
                let height = frame.height();
                // Normalize any backing pixel format (packed RGBA/BGRA, GPU,
                // NV12) into tightly-packed RGBA8, then swizzle to BGRA8
                // during copy-out -- see HOLOIROH_PIXFMT_BGRA8's doc for why
                // (iOS CoreVideo has no 32RGBA pool support; feeding Swift
                // RGBA made every CVPixelBufferPool creation fail silently).
                let rgba = frame.rgba_image();
                let bytes: &[u8] = rgba.as_raw();
                let len = bytes.len();

                // Fill metadata first so a BUFFER_TOO_SMALL caller learns the
                // real dimensions to size a retry buffer.
                unsafe {
                    (*out_frame).width = width;
                    (*out_frame).height = height;
                    (*out_frame).timestamp_us = frame.timestamp.as_micros() as u64;
                    (*out_frame).pixel_format = HOLOIROH_PIXFMT_BGRA8;
                    (*out_frame).kind = 0; // video
                }

                if len > buf_capacity {
                    return HOLOIROH_ERR_BUFFER_TOO_SMALL;
                }
                if len > 0 {
                    // buf may be null only if buf_capacity (and thus len) is 0.
                    if buf.is_null() {
                        return HOLOIROH_ERR_NULL_ARG;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, len);
                        // RGBA -> BGRA in place: swap byte 0 (R) and byte 2
                        // (B) of every pixel. ~3.7MB at 720p, trivially fast
                        // in release; keeps the conversion at the single
                        // choke point both platforms already pass through.
                        let out = std::slice::from_raw_parts_mut(buf, len);
                        for px in out.chunks_exact_mut(4) {
                            px.swap(0, 2);
                        }
                    }
                }
                len as c_int
            }
        }
    }));

    match result {
        Ok(status) => status,
        Err(_) => HOLOIROH_ERR_PANIC,
    }
}

// ---------------------------------------------------------------------
// Control channel (PROTOCOL.md ClientMessage / ServerMessage)
// ---------------------------------------------------------------------

/// Establishes the control channel to the ticket peer.
/// The function dials [`CONTROL_ALPN`] and opens one bidirectional QUIC stream.
/// It performs the personal identification number (PIN) handshake.
/// It waits at most [`CONTROL_HANDSHAKE_TIMEOUT`] for the first daemon reply.
///
/// # Wire contract
///
/// - The app sends `{"type":"pin","pin":"..."}` as a bare newline-delimited JSON (NDJSON) line.
///   An unrecognized device must send this as its first line.
///   The daemon rejects an envelope-wrapped PIN.
/// - On success, the daemon sends an envelope containing `{"type":"status","text":"control channel ready"}`.
///   The function returns [`HOLOIROH_OK`] and stores the send stream.
///   It starts a reader task for later NDJSON lines.
/// - On authentication failure, the daemon sends a bare `{"type":"auth_rejected","text":...}` line.
///   The function returns [`HOLOIROH_ERR_CONNECT_FAILED`].
///   It stores the daemon's reason through `out_error`.
///   Timeout and early closure return the same status.
///
/// After authentication, both directions use signed envelope JSON from `PROTOCOL.md`.
/// [`holoiroh_ios_bridge_control_send`] accepts an unsigned `TaskEnvelope<ClientMessage>`.
/// It validates the session and sequence before signing with the bridge endpoint identity.
/// The reader verifies each daemon envelope against the authenticated transport peer.
/// It queues verified JSON for [`holoiroh_ios_bridge_poll_control_event`].
///
/// An existing connection makes this function return [`HOLOIROH_OK`] immediately.
/// One lock serializes concurrent connection attempts.
/// A prior [`holoiroh_ios_bridge_ticket_connect`] call must have stored the peer address.
/// Otherwise, this function returns [`HOLOIROH_ERR_NOT_CONNECTED`].
/// The function blocks the calling thread.
/// Call it from a background queue.
///
/// For an allowlisted device, the daemon does not consume the bare PIN during authentication.
/// Its envelope loop later returns one `{"type":"error","text":"malformed envelope: ..."}` envelope.
/// [`holoiroh_ios_bridge_poll_control_event`] reports that harmless envelope once.
/// The ready greeting arrives first, so this function still returns [`HOLOIROH_OK`].
///
/// # Safety
///
/// `bridge` must be a live pointer returned by a bridge constructor.
/// `pin_cstr` must remain a valid, null-terminated C string during this call.
/// `out_error` uses the exact `*mut *mut c_char` contract from [`holoiroh_ios_bridge_ticket_connect`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_control_connect(
    bridge: *mut HoloirohBridge,
    pin_cstr: *const c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(inner) = (unsafe { bridge_ref(bridge) }) else {
            unsafe { set_error(out_error, "bridge pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        };
        if pin_cstr.is_null() {
            unsafe { set_error(out_error, "pin string pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        }
        let pin = match unsafe { CStr::from_ptr(pin_cstr) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                unsafe { set_error(out_error, "pin string is not valid UTF-8") };
                return HOLOIROH_ERR_UNKNOWN;
            }
        };

        // Idempotency + single-dialer: the control slot's lock is held for
        // the whole connect, so a second caller either sees the stored
        // state (returns OK) or blocks until the first dial resolves.
        // Holding a std::sync::Mutex guard across `block_on` is sound here:
        // Runtime::block_on polls the future on *this* thread (it has no
        // `Send` bound), and nothing inside the future takes this lock.
        let mut control = inner.control.lock().unwrap_or_else(|e| e.into_inner());
        if control.is_some() && !inner.control_ended.load(Ordering::Acquire) {
            return HOLOIROH_OK;
        }
        if let Some(previous) = control.take() {
            previous.connection.close(0u32.into(), b"control reconnect");
        }
        let generation = inner.control_generation.fetch_add(1, Ordering::AcqRel) + 1;
        inner.control_ended.store(false, Ordering::Release);
        inner
            .control_events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        *inner
            .control_error
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;

        let Some(peer) = inner
            .control_peer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            unsafe {
                set_error(
                    out_error,
                    "no peer to dial: call holoiroh_ios_bridge_ticket_connect first",
                )
            };
            return HOLOIROH_ERR_NOT_CONNECTED;
        };

        // Bare (non-envelope) PIN line -- see this function's doc. serde_json
        // handles JSON string escaping of arbitrary PIN content.
        let mut pin_line = serde_json::json!({ "type": "pin", "pin": pin }).to_string();
        pin_line.push('\n');

        // First line of defense against the on-device "aborted by peer" race
        // (see the retry loop below for the second): if the media
        // subscription's connection to this SAME peer is live, grab a clone of
        // it so we can wait for its path set to settle before dialing the
        // control connection. iroh 1.0.x removed every endpoint-level
        // path-state watcher (0.92's `Endpoint::conn_type()` is gone;
        // `remote_info()` is an explicit non-watching snapshot), so the only
        // real path-readiness signal left is the existing media Connection's
        // own `paths()` -- the same is_ip()/is_relay()/is_selected() idiom
        // iroh's own `test_paths_watcher` uses to wait for path stabilization.
        // `Connection` is a cheap Arc-backed clone, taken while holding the
        // subscription lock only momentarily (never across an await).
        let media_conn = inner
            .subscription
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.session().conn().clone());

        let local_public = inner.live.endpoint().secret_key().public();
        let connect_result = inner.runtime.block_on(async {
            // Wait (bounded, 1.5s) for the media connection's path set to
            // settle: either a direct (IP) path exists, or a relay path has
            // been SELECTED as the transmission path -- not merely opened.
            // Dialing the second QUIC connection mid-hole-punch is what races
            // the path discovery and produces the transport-level abort; by
            // the time the FIRST connection's paths have settled, the peer's
            // address resolution is warm for the second dial. On timeout we
            // fall through unchanged -- the retry loop below remains the
            // safety net for any residual race window.
            if let Some(conn) = &media_conn {
                let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
                loop {
                    let settled = conn
                        .paths()
                        .iter()
                        .any(|p| p.is_ip() || (p.is_relay() && p.is_selected()));
                    if settled || tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }

            // Connection establishment (dial + open_bi) is retried up to
            // MAX_CONNECT_ATTEMPTS times: on the physical device this half
            // has been observed to fail with a transport-level QUIC abort
            // (quinn-proto ConnectionError::ConnectionClosed, "aborted by
            // peer") that does not reproduce on loopback-adjacent (Mac-to-
            // Mac) runs of the identical code path -- consistent with a
            // direct-path/NAT-traversal race rather than a real rejection
            // (the daemon's own logs show QAD-observed address variance and
            // HostUnreachable during hole-punch attempts on flaky networks).
            // A short escalating backoff gives that race a beat to resolve.
            // Each attempt is a *fresh* connect() call -- a half-failed
            // connection/stream is never reused across attempts.
            //
            // Deliberately NOT retried: everything from the PIN write
            // onward (write/flush/greeting-read below). A real
            // `auth_rejected`, a malformed greeting, or a genuine handshake
            // timeout must surface immediately on the first occurrence --
            // retrying those would silently mask an authentication failure,
            // which is a behavior regression, not resilience.
            const MAX_CONNECT_ATTEMPTS: u32 = 3;
            const CONNECT_RETRY_BACKOFF: [Duration; 2] =
                [Duration::from_millis(300), Duration::from_millis(700)];

            let mut connect_attempt_errors: Vec<String> = Vec::new();
            let (connection, mut send, recv) = 'connect: loop {
                let attempt = connect_attempt_errors.len() as u32 + 1;
                let dial_result: Result<_, String> = async {
                    let connection = inner
                        .live
                        .endpoint()
                        .connect(peer.clone(), CONTROL_ALPN)
                        .await
                        .map_err(|err| format!("control-channel connect failed: {err}"))?;
                    let (send, recv) = connection
                        .open_bi()
                        .await
                        .map_err(|err| format!("control-channel open_bi failed: {err}"))?;
                    Ok((connection, send, recv))
                }
                .await;

                match dial_result {
                    Ok(parts) => break 'connect parts,
                    Err(msg) => {
                        // Transport-level failure of the dial/open_bi half
                        // only -- not an application-level rejection (those
                        // can only occur after the PIN line is sent, i.e.
                        // below this loop). Trace-log-equivalent: recorded
                        // here as a comment/attempt-history since this crate
                        // has no tracing subscriber wired up on iOS; the
                        // full history is folded into the returned error on
                        // final failure so a future diagnosis has evidence.
                        connect_attempt_errors.push(format!("attempt {attempt}: {msg}"));
                        if attempt >= MAX_CONNECT_ATTEMPTS {
                            return Err(format!(
                                "control-channel connect failed after {attempt} attempts: {}",
                                connect_attempt_errors.join("; ")
                            ));
                        }
                        tokio::time::sleep(CONNECT_RETRY_BACKOFF[(attempt - 1) as usize]).await;
                        continue 'connect;
                    }
                }
            };

            send.write_all(pin_line.as_bytes())
                .await
                .map_err(|err| format!("control-channel PIN write failed: {err}"))?;
            send.flush()
                .await
                .map_err(|err| format!("control-channel PIN flush failed: {err}"))?;

            // First reply line: bare auth_rejected, or the envelope-wrapped
            // ready greeting.
            let mut lines = BufReader::new(recv);
            let greeting = tokio::time::timeout(CONTROL_HANDSHAKE_TIMEOUT, async {
                loop {
                    match read_bounded_control_line(&mut lines).await {
                        Ok(Some(line)) if line.trim().is_empty() => continue,
                        Ok(Some(line)) => break Ok(line),
                        Ok(None) => {
                            break Err(
                                "connection closed before control-channel greeting (auth rejected \
                                 without a reason, or daemon shut down)"
                                    .to_string(),
                            );
                        }
                        Err(error) => break Err(error),
                    }
                }
            })
            .await
            .map_err(|_| {
                format!(
                    "timed out after {}s waiting for control-channel greeting",
                    CONTROL_HANDSHAKE_TIMEOUT.as_secs()
                )
            })??;

            let value: serde_json::Value = serde_json::from_str(&greeting)
                .map_err(|err| format!("unparseable control-channel greeting: {err}"))?;
            if value.get("type").and_then(|t| t.as_str()) == Some("auth_rejected") {
                let reason = value
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("(no reason given)");
                return Err(format!("auth rejected: {reason}"));
            }

            let shell: TaskEnvelope<serde_json::Value> = serde_json::from_value(value)
                .map_err(|error| format!("invalid signed control-channel greeting: {error}"))?;
            if shell.session_id.is_empty() {
                return Err("control-channel greeting has an empty session_id".to_string());
            }
            if shell.sequence_number != 0 {
                return Err("control-channel greeting must have sequence_number 0".to_string());
            }
            let session_id = shell.session_id.clone();
            let daemon_public = connection.remote_id();
            let mut inbound = InboundEnvelopeState::for_session(session_id.clone());
            let greeting_envelope =
                validate_server_line(&greeting, &daemon_public, &local_public, &mut inbound)?;
            match greeting_envelope.payload {
                ServerMessage::Status { text, .. }
                    if text.as_deref() == Some("control channel ready") => {}
                _ => return Err("daemon did not send the required ready greeting".to_string()),
            }

            Ok::<_, String>((
                connection,
                send,
                lines,
                greeting,
                inbound,
                daemon_public,
                session_id,
            ))
        });

        let (connection, send, lines, greeting, mut inbound, daemon_public, session_id) =
            match connect_result {
                Ok(parts) => parts,
                Err(msg) => {
                    unsafe { set_error(out_error, &msg) };
                    return HOLOIROH_ERR_CONNECT_FAILED;
                }
            };

        // Reader task: every subsequent NDJSON line goes into the shared
        // queue for poll_control_event; EOF or a read error marks the
        // channel ended. Reuses the handshake's `Lines` reader so any bytes
        // it buffered past the greeting are not lost.
        //
        // The greeting line itself (`greeting`, parsed above into `value`
        // purely to check for `auth_rejected`) is queued here too, NOT
        // discarded: it is the envelope-wrapped `TaskEnvelope<ServerMessage>`
        // carrying the daemon-minted `session_id` the Swift side's
        // `HoloConnection.decodeServerLine` needs to populate
        // `OutboundEnvelopeState` before any outbound send can be
        // envelope-wrapped -- see `ControlChannelSender.swift`'s
        // `OutboundEnvelope`/`encoded(_:sessionState:)`. Previously this
        // line was read and thrown away here, so `poll_control_event` never
        // surfaced it and the Swift side's `sessionId` stayed `nil` forever
        // -- live-witnessed as every send failing with "no session_id yet
        // (daemon greeting not received)" even though the daemon logged
        // "session established" and the greeting was sent successfully.
        inner.control_ended.store(false, Ordering::Release);
        let events = inner.control_events.clone();
        events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(greeting);
        let ended = inner.control_ended.clone();
        let errors = inner.control_error.clone();
        let generations = inner.control_generation.clone();
        let reader_connection = connection.clone();
        let reader_daemon_public = daemon_public.clone();
        inner.runtime.spawn(async move {
            let mut lines = lines;
            loop {
                match read_bounded_control_line(&mut lines).await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => {
                        let validation = validate_server_line(
                            &line,
                            &reader_daemon_public,
                            &local_public,
                            &mut inbound,
                        );
                        if let Err(error) = validation {
                            if generations.load(Ordering::Acquire) == generation {
                                events.lock().unwrap_or_else(|e| e.into_inner()).clear();
                                *errors.lock().unwrap_or_else(|e| e.into_inner()) =
                                    Some(bounded_control_error(error));
                                ended.store(true, Ordering::Release);
                                reader_connection.close(0u32.into(), b"invalid signed envelope");
                            }
                            break;
                        }
                        if generations.load(Ordering::Acquire) != generation {
                            break;
                        }
                        events
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_back(line);
                    }
                    Ok(None) => {
                        if generations.load(Ordering::Acquire) == generation {
                            ended.store(true, Ordering::Release);
                        }
                        break;
                    }
                    Err(error) => {
                        if generations.load(Ordering::Acquire) == generation {
                            events.lock().unwrap_or_else(|e| e.into_inner()).clear();
                            *errors.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(bounded_control_error(error));
                            ended.store(true, Ordering::Release);
                            reader_connection.close(0u32.into(), b"invalid control frame");
                        }
                        break;
                    }
                }
            }
        });

        *control = Some(ControlState {
            connection,
            send,
            session_id,
            daemon_public,
            next_outbound_sequence: 0,
        });
        HOLOIROH_OK
    }));

    match result {
        Ok(status) => status,
        Err(_) => {
            unsafe { set_error(out_error, "internal panic during control_connect") };
            HOLOIROH_ERR_PANIC
        }
    }
}

/// Sends one unsigned `TaskEnvelope<ClientMessage>` JSON value from Swift.
/// The bridge rejects caller-supplied signatures and invalid envelope metadata.
/// It binds the envelope to the verified session and next outbound sequence.
/// It signs the envelope with the bridge endpoint's Ed25519 identity.
/// It writes the signed envelope as one newline-delimited JSON (NDJSON) line.
/// The function blocks until the QUIC stream accepts the bytes.
/// Call it from a background queue.
///
/// Before [`holoiroh_ios_bridge_control_connect`] succeeds, it returns [`HOLOIROH_ERR_NOT_CONNECTED`].
/// A write failure returns [`HOLOIROH_ERR_CONNECT_FAILED`].
/// It then drops the stored stream so a later connection call can dial again.
///
/// # Safety
///
/// `bridge` must be a live pointer returned by a bridge constructor.
/// `json_cstr` must remain a valid, null-terminated C string during this call.
/// `out_error` must follow the documented `*mut *mut c_char` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_control_send(
    bridge: *mut HoloirohBridge,
    json_cstr: *const c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(inner) = (unsafe { bridge_ref(bridge) }) else {
            unsafe { set_error(out_error, "bridge pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        };
        if json_cstr.is_null() {
            unsafe { set_error(out_error, "json string pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        }
        let json = match unsafe { CStr::from_ptr(json_cstr) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                unsafe { set_error(out_error, "json string is not valid UTF-8") };
                return HOLOIROH_ERR_UNKNOWN;
            }
        };

        let unsigned_json = json.trim_end_matches(['\r', '\n']);
        let mut envelope: TaskEnvelope<ClientMessage> = match serde_json::from_str(unsigned_json) {
            Ok(envelope) => envelope,
            Err(error) => {
                unsafe {
                    set_error(
                        out_error,
                        &format!(
                            "control_send requires an unsigned TaskEnvelope<ClientMessage>: {error}"
                        ),
                    )
                };
                return HOLOIROH_ERR_UNKNOWN;
            }
        };
        if envelope.signature.is_some() {
            unsafe { set_error(out_error, "caller-supplied signatures are not accepted") };
            return HOLOIROH_ERR_UNKNOWN;
        }
        if envelope.protocol_version != PROTOCOL_VERSION {
            unsafe { set_error(out_error, "unsupported control envelope protocol_version") };
            return HOLOIROH_ERR_UNKNOWN;
        }
        if envelope.message_type != envelope.payload.type_tag() {
            unsafe { set_error(out_error, "control envelope message_type mismatch") };
            return HOLOIROH_ERR_UNKNOWN;
        }

        let mut control = inner.control.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = control.as_mut() else {
            unsafe {
                set_error(
                    out_error,
                    "control channel not connected: call \
                     holoiroh_ios_bridge_control_connect first",
                )
            };
            return HOLOIROH_ERR_NOT_CONNECTED;
        };

        if envelope.session_id != state.session_id {
            unsafe {
                set_error(
                    out_error,
                    "control envelope session_id is not the verified session",
                )
            };
            return HOLOIROH_ERR_UNKNOWN;
        }
        if envelope.sequence_number != state.next_outbound_sequence {
            unsafe {
                set_error(
                    out_error,
                    "control envelope sequence_number is not the next value",
                )
            };
            return HOLOIROH_ERR_UNKNOWN;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        if envelope.is_expired_at(now_ms) {
            unsafe { set_error(out_error, "control envelope is expired") };
            return HOLOIROH_ERR_UNKNOWN;
        }
        if let Err(error) = sign_client_envelope(
            &mut envelope,
            inner.live.endpoint().secret_key(),
            &state.daemon_public,
        ) {
            unsafe { set_error(out_error, &bounded_control_error(error)) };
            return HOLOIROH_ERR_UNKNOWN;
        }
        let mut line = match serde_json::to_string(&envelope) {
            Ok(line) => line,
            Err(error) => {
                unsafe {
                    set_error(
                        out_error,
                        &format!("serializing signed control envelope: {error}"),
                    )
                };
                return HOLOIROH_ERR_UNKNOWN;
            }
        };
        line.push('\n');

        let write_result = inner.runtime.block_on(async {
            state.send.write_all(line.as_bytes()).await?;
            state.send.flush().await?;
            Ok::<(), std::io::Error>(())
        });

        match write_result {
            Ok(()) => {
                state.next_outbound_sequence += 1;
                HOLOIROH_OK
            }
            Err(err) => {
                // The stream is dead: drop the stored state so a later
                // control_connect can re-dial, and mark the channel ended so
                // poll_control_event reports ENDED once the queue drains.
                *control = None;
                inner.control_generation.fetch_add(1, Ordering::AcqRel);
                inner.control_ended.store(true, Ordering::Release);
                unsafe { set_error(out_error, &format!("control-channel write failed: {err}")) };
                HOLOIROH_ERR_CONNECT_FAILED
            }
        }
    }));

    match result {
        Ok(status) => status,
        Err(_) => {
            unsafe { set_error(out_error, "internal panic during control_send") };
            HOLOIROH_ERR_PANIC
        }
    }
}

/// Polls without blocking for the next verified `TaskEnvelope<ServerMessage>` NDJSON line.
/// The reader rejects malformed, unsigned, incorrectly signed, expired, replayed, or out-of-sequence envelopes.
///
/// Return behavior:
///
/// - If a line is queued, the function allocates a null-terminated copy through `out_json`.
///   It returns [`HOLOIROH_OK`].
///   The caller must free the string with [`holoiroh_ios_bridge_free_error_string`].
/// - If the queue is empty and the stream is active, it stores null through `out_json`.
///   It returns [`HOLOIROH_OK`].
/// - If the queue is empty and the stream ended, it stores null through `out_json`.
///   It returns [`HOLOIROH_ERR_ENDED`].
///   [`holoiroh_ios_bridge_control_connect`] can establish a new channel.
///
/// # Safety
///
/// `bridge` must be a live pointer returned by a bridge constructor.
/// `out_json` must be a valid, writable `*mut *mut c_char`.
/// The caller must free any stored string with [`holoiroh_ios_bridge_free_error_string`].
/// `out_error` must follow the same `*mut *mut c_char` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_poll_control_event(
    bridge: *mut HoloirohBridge,
    out_json: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if out_json.is_null() {
            unsafe { set_error(out_error, "out_json pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        }
        unsafe {
            *out_json = std::ptr::null_mut();
        }
        let Some(inner) = (unsafe { bridge_ref(bridge) }) else {
            unsafe { set_error(out_error, "bridge pointer is null") };
            return HOLOIROH_ERR_NULL_ARG;
        };

        if let Some(error) = inner
            .control_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            inner
                .control_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            unsafe { set_error(out_error, &error) };
            return HOLOIROH_ERR_CONNECT_FAILED;
        }

        // Same NOT_CONNECTED contract as `control_send`: an empty queue is ambiguous between
        // "never connected" and "connected, nothing new yet" unless this is checked explicitly.
        // Witnessed live by ffi_probe: without this check a never-connected bridge polled OK (0)
        // with a null out_json instead of reporting it was never connected.
        if inner
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
            unsafe {
                set_error(
                    out_error,
                    "control channel not connected: call holoiroh_ios_bridge_control_connect first",
                );
            }
            return HOLOIROH_ERR_NOT_CONNECTED;
        }

        let popped = inner
            .control_events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front();

        match popped {
            Some(line) => {
                // A NUL byte can't cross as a C string; JSON off this wire
                // never contains one, but strip defensively rather than
                // panicking or silently dropping the whole event.
                let cstring = CString::new(line).unwrap_or_else(|err| {
                    let mut bytes = err.into_vec();
                    bytes.retain(|b| *b != 0);
                    CString::new(bytes).expect("all NUL bytes removed")
                });
                unsafe {
                    *out_json = cstring.into_raw();
                }
                HOLOIROH_OK
            }
            None => {
                if inner.control_ended.load(Ordering::Acquire) {
                    HOLOIROH_ERR_ENDED
                } else {
                    HOLOIROH_OK
                }
            }
        }
    }));

    match result {
        Ok(status) => status,
        Err(_) => {
            unsafe { set_error(out_error, "internal panic during poll_control_event") };
            HOLOIROH_ERR_PANIC
        }
    }
}

// ---------------------------------------------------------------------
// Shared teardown helper
// ---------------------------------------------------------------------

/// Frees a C string that this crate allocated through an `out_error` or `out_json` parameter.
/// [`holoiroh_ios_bridge_ticket_connect`] is one function that can return such a string.
/// A null pointer has no effect.
///
/// Use this function for every Rust-allocated string that crosses this foreign function interface (FFI) boundary.
/// Do not use Swift memory management or call libc `free` directly.
/// `CString::into_raw` used Rust's global allocator to create the string.
/// The corresponding Rust allocator must deallocate it.
///
/// # Safety
///
/// `s` must be null or an unfreed pointer returned through this crate's `out_error` or `out_json` parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_free_error_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(unsafe { CString::from_raw(s) });
    }));
}
