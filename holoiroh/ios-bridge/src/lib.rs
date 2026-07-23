//! `extern "C"` FFI bridge from `iroh-live` to the iOS Swift client.
//!
//! # Why this crate exists
//!
//! Neither `iroh` nor `iroh-live` ships official Swift/UniFFI bindings for
//! `iroh-live`'s media API (base `iroh` *does* have official Swift bindings,
//! via the separate `n0-computer/iroh-ffi` repo -- but that wraps raw
//! `Endpoint`/`Connection`, not `iroh-live`'s `LocalBroadcast`/`subscribe`/
//! frame-pull surface, which is what this project actually needs). Full
//! research trail, sources, and the exact decision rationale live in
//! `../ios/IROH_FFI.md` -- read that first if you're wondering "why not just
//! use iroh-ffi directly."
//!
//! This crate is the fallback plan: a minimal hand-written bridge, following
//! the same shape `iroh-live`'s own `moq-media-android` crate uses for
//! Kotlin/JNI (a hand-rolled bridge crate, not a code-generated one) --
//! that's an established precedent in the upstream project, not an unusual
//! choice made here.
//!
//! # Status: real subscribe implementation (as-built)
//!
//! Every `extern "C"` function below has a real body wired to the actual
//! `iroh-live` subscribe API, verified against the vendored crate source at
//! commit `5f95758` (the same commit `Cargo.toml` pins):
//!
//! - Connect: [`iroh::Endpoint::builder`]`(presets::N0).bind()` ->
//!   [`iroh_live::Live::builder`]`(ep).with_router().spawn()`.
//! - Ticket: [`iroh_live::ticket::LiveTicket::from_str`] -> a struct with a
//!   public `endpoint: EndpointAddr` and `broadcast_name: String`.
//! - Subscribe: [`iroh_live::Live::subscribe`]`(ticket.endpoint,
//!   &ticket.broadcast_name)` -> [`iroh_live::Subscription`].
//! - Video track: `subscription.broadcast().video_ready()` ->
//!   `moq_media::subscribe::VideoTrack` (waits for the catalog to advertise a
//!   video rendition, then subscribes to the best quality, decoding via the
//!   platform's default decoder -- VideoToolbox on Apple targets).
//! - Frames: `VideoTrack::try_recv()` (non-blocking) ->
//!   `Option<moq_media::format::VideoFrame>`; `frame.rgba_image().as_raw()`
//!   normalizes any backing pixel format (packed RGBA/BGRA, GPU, NV12) into a
//!   tightly-packed `width * height * 4` RGBA8 byte buffer that maps directly
//!   onto a `kCVPixelFormatType_32RGBA` `CVPixelBuffer` on the Swift side.
//!
//! The control-channel functions (`holoiroh_ios_bridge_control_connect` /
//! `_control_send` / `_poll_control_event`) ride a **separate** iroh ALPN
//! (`holoiroh/control/1`, byte-identical to
//! `mac-daemon/src/control_channel.rs`'s `CONTROL_ALPN`), not the media
//! subscribe path: `_control_connect` dials the same peer the ticket named
//! (via `live.endpoint().connect(peer, CONTROL_ALPN)`), opens one
//! bidirectional stream, performs the bare-line PIN handshake the daemon's
//! auth gate expects, then hands the recv half to a reader task feeding an
//! NDJSON event queue -- see each function's doc comment for the exact wire
//! contract.
//!
//! # FFI design notes
//!
//! - **Opaque handles, not exposed structs.** Every stateful object
//!   (`Bridge`, `Subscription`) crosses the boundary as an opaque pointer
//!   (typed here as a pointer to a zero-sized marker struct, so C/Swift can't
//!   accidentally dereference it) obtained from a `_new`/`_subscribe`
//!   function and released by a matching `_free` function. This is the same
//!   pattern `iroh-ffi` uses internally before uniffi's codegen wraps it in a
//!   Swift class -- we're doing by hand what uniffi would otherwise generate.
//! - **No `async fn` across the FFI boundary.** `async`/`.await` doesn't have
//!   a C ABI. Every `extern "C"` function here is synchronous from the
//!   caller's point of view; internally, `BridgeInner` owns a Tokio
//!   multi-thread runtime and blocks the calling thread on
//!   `runtime.block_on(...)` for connect/subscribe calls, or returns
//!   immediately for polling calls (`poll_next_frame`) which do a
//!   non-blocking `try_recv` on the decoded-frame channel. Swift is expected
//!   to call the connect/subscribe functions and the polling loop from a
//!   background `DispatchQueue`/`Task`, not the main thread.
//! - **No panic may unwind across the boundary.** `extern "C"` functions must
//!   never unwind across the FFI boundary (undefined behavior in Rust; a hard
//!   crash in practice). Every fallible function is wrapped in
//!   [`std::panic::catch_unwind`] and returns a sentinel (null pointer /
//!   negative int) on failure, writing a heap-allocated, null-terminated
//!   error string to an optional `*mut *mut c_char` out-param, freed by the
//!   caller via [`holoiroh_ios_bridge_free_error_string`]. This mirrors what
//!   uniffi generates automatically (`Result<T, IrohError>` -> Swift
//!   `throws`); we do it by hand here.
//! - **Frames are caller-allocated-and-filled, not Rust-allocated-and-
//!   returned**, to avoid an allocation per frame on a hot path (screen
//!   capture at 30-60fps): [`holoiroh_ios_bridge_poll_next_frame`] takes a
//!   caller-owned buffer + capacity and, if the frame doesn't fit, returns
//!   [`HOLOIROH_ERR_BUFFER_TOO_SMALL`] after writing the frame's real
//!   dimensions into `out_frame` (so the caller can size a buffer as
//!   `width * height * 4` and retry), rather than returning an owned
//!   allocation the caller would have to free per-frame.
//!
//! # Packaging this crate into an iOS `.xcframework`
//!
//! See `../ios/IROH_FFI.md`'s "As-built: xcframework packaging" section for
//! the full, witnessed command sequence. In brief:
//!
//! 1. `rustup target add aarch64-apple-ios aarch64-apple-ios-sim
//!    x86_64-apple-ios-sim`
//! 2. `cargo build -p holoiroh-ios-bridge --target <triple> --release`
//!    per target -> `target/<triple>/release/libholoiroh_ios_bridge.a`
//! 3. `lipo -create` the two simulator slices into one fat `.a`.
//! 4. Generate `include/HoloirohIosBridge.h` (committed) + the
//!    `module.modulemap` beside it.
//! 5. `xcodebuild -create-xcframework` combining the device slice + fused
//!    simulator slice, each paired with `-headers include/`.
//! 6. Add the `.xcframework` to the Xcode/SwiftPM target;
//!    `import HoloirohIosBridge` from Swift. `IrohLiveFrameSource`
//!    (`../ios/Sources/HoloIrohApp/Video/IrohLiveFrameSource.swift`) wraps
//!    the C functions into a `VideoFrameSource`.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, SendStream, presets};
use iroh::{Endpoint, EndpointAddr};
use iroh_live::Live;
use iroh_live::media::subscribe::VideoTrack;
use iroh_live::ticket::LiveTicket;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::runtime::Runtime;

/// Opaque handle to a running bridge instance: owns the Tokio runtime, the
/// `iroh_live::Live` session, and (once connected) the current subscription.
/// Obtained via [`holoiroh_ios_bridge_new`], released via
/// [`holoiroh_ios_bridge_free`].
///
/// Zero-sized and never constructed on the Rust side as a real value -- only
/// ever handed out as `Box::into_raw(Box::new(BridgeInner {..}))` cast to
/// this opaque type, so C/Swift can hold and pass the pointer around without
/// any ability to read its layout (the real state lives in the private,
/// non-`#[repr(C)]` [`BridgeInner`] behind it). (Deliberately not a bare
/// `*mut c_void`: giving `HoloirohBridge` and `HoloirohSubscription` distinct
/// named types means Swift/C callers get a type error if they pass the wrong
/// handle to the wrong function, which `c_void` erasure would silently
/// allow.)
#[repr(C)]
pub struct HoloirohBridge {
    _private: [u8; 0],
}

/// Opaque handle to an active video subscription (post-`subscribe`), distinct
/// from [`HoloirohBridge`] so a bridge holds zero or one active video
/// subscriptions independently of its connection lifecycle. Obtained via
/// [`holoiroh_ios_bridge_subscribe`], released via
/// [`holoiroh_ios_bridge_subscription_free`].
#[repr(C)]
pub struct HoloirohSubscription {
    _private: [u8; 0],
}

/// One decoded media frame's metadata handed back across the FFI boundary by
/// [`holoiroh_ios_bridge_poll_next_frame`]. The frame *bytes* are written into
/// a caller-allocated buffer (see module doc, "Frames are
/// caller-allocated-and-filled"); this struct carries the metadata the Swift
/// side needs to hand those bytes to `CVPixelBuffer` /
/// `AVSampleBufferDisplayLayer` correctly.
#[repr(C)]
pub struct HoloirohFrame {
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp, microseconds since the subscription started
    /// (the decoded frame's own `timestamp`, which is `Duration::ZERO` before
    /// the pipeline assigns a PTS).
    pub timestamp_us: u64,
    /// Pixel format of the bytes written into the caller's buffer. Always
    /// [`HOLOIROH_PIXFMT_RGBA8`] in this build: the frame is normalized to
    /// tightly-packed 8-bit RGBA via `VideoFrame::rgba_image()`, so
    /// `bytes_per_row == width * 4` and total length `== width * height * 4`.
    /// Kept as a field (rather than assumed) so the Swift side can pick the
    /// matching `kCVPixelFormatType_*` without guessing, and so a future
    /// zero-copy path can report a different format without an ABI change.
    pub pixel_format: u32,
    /// 0 = video, 1 = audio. Kept as a plain tag rather than a Rust `enum`
    /// crossing FFI, since `#[repr(C)]` enums are still awkward to bind safely
    /// from Swift compared to a plain integer + doc comment. Always `0`
    /// (video) in this build -- audio is not yet pulled across this bridge.
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
/// The requested operation is not supported by this build. No function
/// currently returns this (the control-channel functions, which used to,
/// are implemented as of this build) -- the constant is kept so the C ABI's
/// error-code numbering stays stable.
pub const HOLOIROH_ERR_UNSUPPORTED: HoloirohStatus = -7;
/// A required pointer argument was null.
pub const HOLOIROH_ERR_NULL_ARG: HoloirohStatus = -8;
/// A Rust panic was caught at the FFI boundary (should never happen; the
/// boundary catches it and returns this rather than unwinding into C, which
/// would be undefined behavior).
pub const HOLOIROH_ERR_PANIC: HoloirohStatus = -9;

/// Pixel-format tag for [`HoloirohFrame::pixel_format`]: tightly-packed 8-bit
/// RGBA (R,G,B,A byte order), `width * 4` bytes per row. Historical: no
/// longer emitted by this build -- see [`HOLOIROH_PIXFMT_BGRA8`].
pub const HOLOIROH_PIXFMT_RGBA8: u32 = 0;

/// Pixel-format tag for [`HoloirohFrame::pixel_format`]: tightly-packed 8-bit
/// BGRA (B,G,R,A byte order) -- what [`holoiroh_ios_bridge_poll_next_frame`]
/// actually emits. Maps to Swift's `kCVPixelFormatType_32BGRA`.
///
/// Why BGRA, not RGBA: `kCVPixelFormatType_32RGBA` is NOT a supported
/// CoreVideo pool/IOSurface format on iOS (it is on macOS), so the Swift
/// side's `CVPixelBufferPoolCreate` silently returned nil and dropped EVERY
/// frame -- live-witnessed as a permanent black screen while the on-device
/// decode pipeline was verifiably delivering 20-40fps the whole session
/// (device console: `vdec stats fps=20..40`, zero errors anywhere). 32BGRA
/// is the universally-supported iOS display format, so the bridge swizzles
/// R<->B during copy-out and tags frames with this constant.
pub const HOLOIROH_PIXFMT_BGRA8: u32 = 1;

/// ALPN identifying the control-channel protocol on the daemon's `iroh`
/// `Endpoint`. Re-exported here from [`holoiroh_wire::CONTROL_ALPN`] rather
/// than a local duplicate byte string -- this crate and
/// `mac-daemon/src/control_channel.rs` both import the one definition in
/// the `holoiroh-wire` crate now, instead of each hand-maintaining a copy
/// that had to stay byte-for-byte identical by convention alone. See
/// `holoiroh-wire/src/lib.rs`'s module doc for the wire schema this ALPN
/// carries and why that crate exists. Re-exported (`pub use`, not a plain
/// `use`) so existing call sites in this crate referencing the bare
/// `CONTROL_ALPN` name (no module qualifier) keep resolving unchanged.
pub use holoiroh_wire::CONTROL_ALPN;

/// How long [`holoiroh_ios_bridge_control_connect`] waits for the daemon's
/// first reply line (a bare `auth_rejected`, or the envelope-wrapped ready
/// greeting) after writing the PIN line. Generous enough for a relay-path
/// round trip plus the daemon's allowlist-save on first pairing; the QUIC
/// connect itself has already succeeded by the time this timer starts.
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------
// Private Rust state behind the opaque handles
// ---------------------------------------------------------------------

/// The real state behind a [`HoloirohBridge`] opaque pointer. Never
/// `#[repr(C)]`; only ever reached via `&*(ptr as *const BridgeInner)`.
struct BridgeInner {
    /// Multi-thread Tokio runtime owned by this bridge. Drives every async
    /// `iroh-live` call via `block_on`. Declared last so it drops last (after
    /// `subscription` and `live`), letting shutdown await on it.
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
    /// The live control-channel connection + send stream, once
    /// [`holoiroh_ios_bridge_control_connect`] has succeeded. One lock serves
    /// both idempotency (connect holds it across the whole dial, so
    /// concurrent connect calls serialize instead of double-dialing) and
    /// writes ([`holoiroh_ios_bridge_control_send`] serializes on it).
    control: Mutex<Option<ControlState>>,
    /// NDJSON lines read off the control stream by the reader task spawned
    /// in [`holoiroh_ios_bridge_control_connect`], drained one line per
    /// [`holoiroh_ios_bridge_poll_control_event`] call. `Arc` because the
    /// reader task (spawned on `runtime`, hence `'static`) shares it with
    /// the FFI side.
    control_events: Arc<Mutex<VecDeque<String>>>,
    /// Set by the reader task on stream end (EOF or read error) and by a
    /// failed [`holoiroh_ios_bridge_control_send`]. Once the event queue is
    /// also drained, [`holoiroh_ios_bridge_poll_control_event`] reports
    /// [`HOLOIROH_ERR_ENDED`].
    control_ended: Arc<AtomicBool>,
}

/// The live control-channel transport stored in [`BridgeInner`]'s `control`
/// slot once [`holoiroh_ios_bridge_control_connect`] succeeds. Keeps the
/// [`Connection`] handle alive alongside the send half (an iroh/QUIC
/// connection closes once every handle to it is dropped; the recv half lives
/// inside the reader task, so `connection` here pins it from the FFI side
/// too and gives [`holoiroh_ios_bridge_free`] something to `close()`
/// explicitly).
struct ControlState {
    connection: Connection,
    send: SendStream,
}

/// The real state behind a [`HoloirohSubscription`] opaque pointer: the
/// decoded video track. `try_recv`/`next_frame` need `&mut self`, and the
/// poll may be called from a background Swift thread, so the track lives
/// behind a `Mutex`.
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

// ---------------------------------------------------------------------
// Lifecycle: bridge construction / teardown
// ---------------------------------------------------------------------

/// Creates a new bridge instance: spins up an internal Tokio multi-thread
/// runtime and an `iroh-live` [`Live`] session (binding an iroh
/// [`Endpoint`] with n0's default relay/discovery preset), but does **not**
/// connect to any peer yet (see [`holoiroh_ios_bridge_ticket_connect`]).
/// Returns null on failure (runtime or endpoint construction failed).
///
/// # Safety
/// The returned pointer, if non-null, must eventually be passed to exactly
/// one call of [`holoiroh_ios_bridge_free`]. It must not be dereferenced
/// directly by the caller (opaque type) and must not be used after being
/// freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_new() -> *mut HoloirohBridge {
    // On-device pipeline visibility (Once-guarded, idempotent across
    // repeated bridge_new calls): without this, every media-pipeline
    // tracing event -- decoder selection, per-packet decode errors,
    // pipeline start/stop -- is silently dropped on iOS (no subscriber
    // installed anywhere in the app), which is exactly how the
    // permanent-black-screen bug stayed invisible on-device while the
    // identical code path was fully debuggable on macOS probes. stderr
    // reaches the device console when the app is launched with
    // `devicectl ... launch --console`. `try_init` (not `init`): losing to
    // a race with some other subscriber must never panic across FFI.
    {
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

    let result = catch_unwind(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .ok()?;

        // Bind an iroh endpoint with n0's default relay/discovery preset, then
        // build a Live session with an internal router (so incoming MoQ
        // sessions -- the publisher's catalog/tracks -- are accepted). This is
        // exactly the pattern iroh-live's own subscribe_test.rs / frame_dump.rs
        // examples use.
        let live = runtime.block_on(async {
            let endpoint = Endpoint::builder(presets::N0).bind().await.ok()?;
            Some(Live::builder(endpoint).with_router().spawn())
        })?;

        let inner = Box::new(BridgeInner {
            runtime,
            live,
            subscription: Mutex::new(None),
            control_peer: Mutex::new(None),
            control: Mutex::new(None),
            control_events: Arc::new(Mutex::new(VecDeque::new())),
            control_ended: Arc::new(AtomicBool::new(false)),
        });
        Some(Box::into_raw(inner) as *mut HoloirohBridge)
    });

    match result {
        Ok(Some(ptr)) => ptr,
        // Construction failure or a caught panic -> null (never unwind into C).
        Ok(None) | Err(_) => std::ptr::null_mut(),
    }
}

/// Releases a bridge instance created by [`holoiroh_ios_bridge_new`], tearing
/// down any active subscription, its `iroh_live::Live` session (via
/// `live.shutdown().await`), and its Tokio runtime. Passing null is a no-op
/// (matches `free(NULL)` C convention).
///
/// # Safety
/// `bridge` must either be null or a pointer previously returned by
/// [`holoiroh_ios_bridge_new`] and not already freed. The caller must not use
/// `bridge` again after this call. Any [`HoloirohSubscription`] obtained from
/// this bridge must be freed *before* this call (the subscription's video
/// track is driven by the bridge's runtime).
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
            let mut sub = inner
                .subscription
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *sub = None;
        }
        // Close the control channel (if connected) before shutting the Live
        // session down; the reader task ends on its own when the closed
        // stream EOFs, and is torn down with the runtime regardless.
        {
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

/// ADDITIVE, fully self-contained daemon-reachability probe. Touches NONE of the
/// existing bridge/connection state: it binds a THROWAWAY iroh endpoint (its own
/// identity, so it never fights the daemon's pkarr record), dials the ticket's
/// node on [`CONTROL_ALPN`], and reports whether a control bi-stream opens within
/// `timeout_ms`.
///
/// A successful dial + `open_bi` means the daemon is up and accepting control
/// connections -- exactly what the pairing screen's "reachable / unreachable"
/// indicator and the launch auto-connect want to know BEFORE a real connect.
/// (The PIN handshake happens later on a real connect; it is not needed just to
/// learn that the daemon is reachable, and the daemon accepts the connection +
/// stream before the PIN line anyway.)
///
/// Returns `true` if reachable, `false` otherwise (null/invalid ticket, no
/// runtime, endpoint bind failure, dial timeout, or daemon down). Blocks the
/// calling thread for at most ~`timeout_ms` plus a brief endpoint bind, so call
/// it off the main thread. Never panics across the FFI boundary.
///
/// # Safety
/// `ticket_cstr` must be a valid null-terminated C string, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_probe_reachable(
    ticket_cstr: *const c_char,
    timeout_ms: u64,
) -> bool {
    let result = catch_unwind(AssertUnwindSafe(|| {
        if ticket_cstr.is_null() {
            return false;
        }
        let ticket_str = match unsafe { CStr::from_ptr(ticket_cstr) }.to_str() {
            Ok(s) => s,
            Err(_) => return false,
        };
        let ticket = match LiveTicket::from_str(ticket_str) {
            Ok(t) => t,
            Err(_) => return false,
        };
        // A short-lived, single-thread runtime, isolated to this probe.
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(_) => return false,
        };
        runtime.block_on(async move {
            let endpoint = match Endpoint::builder(presets::N0).bind().await {
                Ok(e) => e,
                Err(_) => return false,
            };
            let peer = ticket.endpoint.clone();
            let dial = async {
                let connection = endpoint
                    .connect(peer, CONTROL_ALPN)
                    .await
                    .map_err(|_| ())?;
                connection.open_bi().await.map_err(|_| ())?;
                Ok::<(), ()>(())
            };
            matches!(
                tokio::time::timeout(Duration::from_millis(timeout_ms.max(500)), dial).await,
                Ok(Ok(()))
            )
        })
    }));
    result.unwrap_or(false)
}

// ---------------------------------------------------------------------
// Ticket-connect
// ---------------------------------------------------------------------

/// Parses an `iroh-live:` ticket string (the format
/// [`LiveTicket`](iroh_live::ticket::LiveTicket) serializes to -- see
/// `../ios/IROH_FFI.md`'s Finding (b)) and connects the bridge's
/// `iroh_live::Live` session to the peer it describes, subscribing to the
/// named broadcast. Blocks the calling thread until the connection attempt
/// resolves (success or failure) -- call from a background queue, not the main
/// thread.
///
/// `ticket_cstr` must be a null-terminated UTF-8 C string, e.g. as produced by
/// Swift's `String.withCString`. `out_error` may be null if the caller doesn't
/// want a human-readable error message on failure (the [`HoloirohStatus`]
/// return value alone still distinguishes failure modes).
///
/// Returns [`HOLOIROH_OK`] on success, or a negative [`HoloirohStatus`] (see
/// constants above) on failure.
///
/// # Safety
/// `bridge` must be a live pointer from [`holoiroh_ios_bridge_new`].
/// `ticket_cstr` must be a valid null-terminated C string for the duration of
/// this call. If non-null, `out_error` must be a valid, writable
/// `*mut c_char` slot; any string written there must later be freed via
/// [`holoiroh_ios_bridge_free_error_string`].
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
            let mut peer = inner
                .control_peer
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *peer = Some(ticket.endpoint.clone());
        }

        // 3. Connect + subscribe on the runtime. This dials the publisher
        //    (direct P2P with NAT hole-punch, relay fallback) and subscribes to
        //    the named broadcast.
        let subscribe_result = inner.runtime.block_on(async {
            inner
                .live
                .subscribe(ticket.endpoint.clone(), &ticket.broadcast_name)
                .await
        });

        match subscribe_result {
            Ok(subscription) => {
                let mut slot = inner
                    .subscription
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
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

/// Begins consuming the video track of the broadcast on an already-connected
/// bridge (see [`holoiroh_ios_bridge_ticket_connect`]), returning an opaque
/// subscription handle that [`holoiroh_ios_bridge_poll_next_frame`] reads
/// from. Internally calls `subscription.broadcast().video_ready().await`,
/// which blocks until the publisher's catalog advertises at least one video
/// rendition, then subscribes to the best-quality rendition and starts the
/// decoder pipeline (VideoToolbox on Apple targets).
///
/// Blocks the calling thread until a video rendition is available -- call from
/// a background queue. Returns null on failure (bridge not connected yet, or
/// the broadcast never advertised a video track); check `out_error` if
/// non-null.
///
/// # Safety
/// `bridge` must be a live, connected pointer (post successful
/// [`holoiroh_ios_bridge_ticket_connect`]). `out_error` follows the same
/// contract as in [`holoiroh_ios_bridge_ticket_connect`]. The returned
/// pointer, if non-null, must eventually be passed to exactly one call of
/// [`holoiroh_ios_bridge_subscription_free`], and *before* the parent bridge
/// is freed.
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
            let slot = inner
                .subscription
                .lock()
                .unwrap_or_else(|e| e.into_inner());
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

/// Non-blocking poll for the latest decoded video frame on an active
/// subscription. Fills `out_frame` (metadata) and copies the frame's RGBA8
/// bytes into caller-owned `buf`, returning the number of bytes actually
/// written into `buf` on success.
///
/// Semantics (all non-negative returns are byte counts; negative returns are
/// [`HoloirohStatus`] errors):
/// - **No frame available yet** -> returns `0`, `out_frame` left untouched.
///   Not an error; poll again shortly (e.g. on a timer-driven loop on a
///   background queue). Internally this is `VideoTrack::try_recv()` returning
///   `None`, which also drains older buffered frames so you always get the
///   most recent decoded frame (the low-latency "latest frame wins" behavior a
///   live mirror wants).
/// - **A frame is available and fits** -> copies `width * height * 4` RGBA8
///   bytes into `buf`, fills `out_frame`, returns that byte count.
/// - **A frame is available but `buf_capacity` is too small** -> returns
///   [`HOLOIROH_ERR_BUFFER_TOO_SMALL`] and writes the frame's real
///   `width`/`height`/`pixel_format` into `out_frame` so the caller can size a
///   `width * height * 4` buffer and poll again. (The frame itself is
///   consumed; the caller re-polls for the next one -- acceptable for a live
///   mirror where a single dropped frame is invisible, and the caller should
///   size its buffer to the largest expected resolution up front to avoid this
///   path entirely.)
/// - **The track has ended** (publisher dropped it) -> returns
///   [`HOLOIROH_ERR_ENDED`]; no further frames will arrive.
///
/// # Safety
/// `subscription` must be a live pointer from
/// [`holoiroh_ios_bridge_subscribe`]. `buf` must be valid and writable for
/// `buf_capacity` bytes (or null iff `buf_capacity` is 0). `out_frame` must be
/// a valid, writable `*mut HoloirohFrame`.
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

/// Establishes the control channel to the Mac daemon the bridge is
/// ticket-connected to: dials the peer stored by
/// [`holoiroh_ios_bridge_ticket_connect`] on [`CONTROL_ALPN`], opens one
/// bidirectional QUIC stream, performs the PIN handshake, and waits (up to
/// [`CONTROL_HANDSHAKE_TIMEOUT`]) for the daemon's first reply line.
///
/// Wire contract (mirror of `mac-daemon/src/control_channel.rs`):
/// - The PIN goes out as a **bare** (non-envelope) NDJSON line
///   `{"type":"pin","pin":"..."}` -- exactly what the daemon's
///   `ControlChannel::authenticate` gate requires as the very first line
///   from an unrecognized device. (The gate would reject an envelope-
///   wrapped PIN, so this one line is deliberately not enveloped.)
/// - On success the daemon's first line is its envelope-wrapped greeting
///   (`payload` = `{"type":"status","text":"control channel ready"}`) ->
///   returns [`HOLOIROH_OK`], stores the send stream on the bridge, and
///   spawns a reader task queueing every subsequent NDJSON line for
///   [`holoiroh_ios_bridge_poll_control_event`].
/// - On auth failure the daemon's first (and only) line is a **bare**
///   `{"type":"auth_rejected","text":...}` -> returns
///   [`HOLOIROH_ERR_CONNECT_FAILED`] with the daemon's reason in
///   `*out_error`. Timeout / early close also map to
///   [`HOLOIROH_ERR_CONNECT_FAILED`].
///
/// Everything *after* the handshake is envelope-framed both directions
/// (`TaskEnvelope<ClientMessage>` / `TaskEnvelope<ServerMessage>`, see
/// `PROTOCOL.md`): [`holoiroh_ios_bridge_control_send`] passes caller-built
/// envelope lines through verbatim, and the lines handed back by
/// [`holoiroh_ios_bridge_poll_control_event`] are envelope JSON for the
/// Swift side to decode.
///
/// Idempotent: returns [`HOLOIROH_OK`] immediately if already connected
/// (checked and connected under one lock, so concurrent callers serialize
/// rather than double-dial). Requires a prior successful
/// [`holoiroh_ios_bridge_ticket_connect`] (which stores the peer address)
/// -- returns [`HOLOIROH_ERR_NOT_CONNECTED`] otherwise. Blocks the calling
/// thread; call from a background queue.
///
/// Note: for a device the daemon has *already allowlisted*, the daemon's
/// auth gate never consumes the PIN line; it instead falls through to the
/// daemon's main envelope loop, which replies with one harmless
/// `{"type":"error","text":"malformed envelope: ..."}` envelope that will
/// surface once via [`holoiroh_ios_bridge_poll_control_event`]. This is
/// cosmetic: the ready greeting is written before that loop reads anything,
/// so it still arrives first and this function still returns
/// [`HOLOIROH_OK`].
///
/// # Safety
/// `bridge` must be a live pointer from [`holoiroh_ios_bridge_new`].
/// `pin_cstr` must be a valid null-terminated C string for the duration of
/// this call. `out_error` follows the same contract as in
/// [`holoiroh_ios_bridge_ticket_connect`].
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
        if control.is_some() {
            return HOLOIROH_OK;
        }

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
            let mut lines = BufReader::new(recv).lines();
            let greeting = tokio::time::timeout(CONTROL_HANDSHAKE_TIMEOUT, async {
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) if line.trim().is_empty() => continue,
                        Ok(Some(line)) => break Ok(line),
                        Ok(None) => {
                            break Err(
                                "connection closed before control-channel greeting (auth rejected \
                                 without a reason, or daemon shut down)"
                                    .to_string(),
                            );
                        }
                        Err(err) => {
                            break Err(format!(
                                "read error awaiting control-channel greeting: {err}"
                            ));
                        }
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
            // Anything else this early is the daemon's post-auth greeting
            // envelope (its accept loop writes `status: "control channel
            // ready"` immediately after the auth gate passes, before reading
            // anything) -- matched structurally rather than on the greeting
            // text, so a daemon-side wording tweak can't break pairing.
            // `greeting` itself is threaded out here (not dropped) so the
            // caller can queue it for `poll_control_event` -- see the
            // `events.push_back(greeting)` call below for why.
            Ok::<_, String>((connection, send, lines, greeting))
        });

        let (connection, send, lines, greeting) = match connect_result {
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
        inner.runtime.spawn(async move {
            let mut lines = lines;
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        events
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push_back(line);
                    }
                    Ok(None) | Err(_) => {
                        ended.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });

        *control = Some(ControlState { connection, send });
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

/// Sends one NDJSON line (per `../PROTOCOL.md`: a `TaskEnvelope<
/// ClientMessage>` the Swift side has already serialized -- this function is
/// transport-only and does not inspect or re-frame the JSON) over the
/// control channel to the connected Mac daemon, appending the terminating
/// `\n` if the caller didn't include one. Blocks until the bytes are
/// accepted by the QUIC stream; call from a background queue.
///
/// Returns [`HOLOIROH_ERR_NOT_CONNECTED`] until
/// [`holoiroh_ios_bridge_control_connect`] has succeeded. On a write
/// failure (peer gone, connection lost) returns
/// [`HOLOIROH_ERR_CONNECT_FAILED`] and drops the stored stream so a later
/// `control_connect` call can re-dial.
///
/// # Safety
/// `bridge` must be a live pointer from [`holoiroh_ios_bridge_new`].
/// `json_cstr` must be a valid null-terminated C string for the duration of
/// this call. `out_error` follows the same contract as elsewhere in this
/// module.
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

        // NDJSON framing: exactly one trailing newline.
        let mut line = json.trim_end_matches(['\r', '\n']).to_owned();
        line.push('\n');

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

        let write_result = inner.runtime.block_on(async {
            state.send.write_all(line.as_bytes()).await?;
            state.send.flush().await?;
            Ok::<(), std::io::Error>(())
        });

        match write_result {
            Ok(()) => HOLOIROH_OK,
            Err(err) => {
                // The stream is dead: drop the stored state so a later
                // control_connect can re-dial, and mark the channel ended so
                // poll_control_event reports ENDED once the queue drains.
                *control = None;
                inner.control_ended.store(true, Ordering::Release);
                unsafe {
                    set_error(out_error, &format!("control-channel write failed: {err}"))
                };
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

/// Non-blocking poll for the next NDJSON line (per `../PROTOCOL.md`: a
/// `TaskEnvelope<ServerMessage>` -- or, rarely, a bare `ServerMessage` if
/// the daemon replied outside a session) received on the control channel.
///
/// Semantics:
/// - **A line is queued** -> writes a freshly-allocated null-terminated
///   copy to `*out_json` (caller frees via
///   [`holoiroh_ios_bridge_free_error_string`]) and returns [`HOLOIROH_OK`].
/// - **Queue empty, stream alive** -> sets `*out_json` to null and returns
///   [`HOLOIROH_OK`]; poll again shortly.
/// - **Queue empty, stream ended** (daemon closed it, or a prior
///   `control_send` failed) -> sets `*out_json` to null and returns
///   [`HOLOIROH_ERR_ENDED`]; a fresh
///   [`holoiroh_ios_bridge_control_connect`] may re-establish the channel.
///
/// # Safety
/// `bridge` must be a live pointer from [`holoiroh_ios_bridge_new`].
/// `out_json` must be a valid writable `*mut *mut c_char`; the string
/// written there (if any) must later be freed via
/// [`holoiroh_ios_bridge_free_error_string`]. `out_error` follows the same
/// contract as elsewhere in this module.
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

/// Frees a C string previously allocated by this crate and handed back through
/// an `out_error`/`out_json` out-parameter (e.g. from
/// [`holoiroh_ios_bridge_ticket_connect`]). Passing null is a no-op.
///
/// Every Rust-allocated string crossing this FFI boundary **must** be freed
/// via this function, never via Swift's own memory management or libc `free`
/// directly -- the allocator that created it (Rust's global allocator, via
/// `CString::into_raw`) must also be the one that deallocates it.
///
/// # Safety
/// `s` must either be null or a pointer previously returned in an
/// `out_error`/`out_json` parameter by a function in this crate, and must not
/// have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_free_error_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(unsafe { CString::from_raw(s) });
    }));
}
