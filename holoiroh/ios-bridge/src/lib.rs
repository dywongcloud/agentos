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
//! The control-channel functions (`holoiroh_ios_bridge_control_send` /
//! `_poll_control_event`) are a **separate** iroh ALPN
//! (`holoiroh/control/1`), not part of the media subscribe path, and are
//! honestly reported as unsupported (returning [`HOLOIROH_ERR_UNSUPPORTED`]
//! without panicking) until the Swift-side control transport is built -- see
//! their doc comments.
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

use std::ffi::{CStr, CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::Mutex;

use iroh::Endpoint;
use iroh::endpoint::presets;
use iroh_live::Live;
use iroh_live::media::subscribe::VideoTrack;
use iroh_live::ticket::LiveTicket;
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
/// The requested operation is not supported by this build (e.g. the
/// control-channel functions, whose transport is separate follow-on work).
pub const HOLOIROH_ERR_UNSUPPORTED: HoloirohStatus = -7;
/// A required pointer argument was null.
pub const HOLOIROH_ERR_NULL_ARG: HoloirohStatus = -8;
/// A Rust panic was caught at the FFI boundary (should never happen; the
/// boundary catches it and returns this rather than unwinding into C, which
/// would be undefined behavior).
pub const HOLOIROH_ERR_PANIC: HoloirohStatus = -9;

/// Pixel-format tag for [`HoloirohFrame::pixel_format`]: tightly-packed 8-bit
/// RGBA (R,G,B,A byte order), `width * 4` bytes per row. Maps to Swift's
/// `kCVPixelFormatType_32RGBA`.
pub const HOLOIROH_PIXFMT_RGBA8: u32 = 0;

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
        inner.runtime.block_on(async {
            inner.live.shutdown().await;
        });
        // `inner` (and thus its runtime) drops here.
    }));
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
                // NV12) into tightly-packed RGBA8. This is the single, stable
                // byte surface handed to Swift.
                let rgba = frame.rgba_image();
                let bytes: &[u8] = rgba.as_raw();
                let len = bytes.len();

                // Fill metadata first so a BUFFER_TOO_SMALL caller learns the
                // real dimensions to size a retry buffer.
                unsafe {
                    (*out_frame).width = width;
                    (*out_frame).height = height;
                    (*out_frame).timestamp_us = frame.timestamp.as_micros() as u64;
                    (*out_frame).pixel_format = HOLOIROH_PIXFMT_RGBA8;
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

/// Sends one `ClientMessage` (per `../PROTOCOL.md`) over the control channel
/// to the connected Mac daemon.
///
/// **Not implemented in this build.** The control channel is a *separate* iroh
/// ALPN (`holoiroh/control/1`, see `../mac-daemon/src/control_channel.rs`),
/// distinct from the media subscribe path this crate's video FFI wires. The
/// iroh-live `Live` session owns the endpoint through its own MoQ router;
/// opening a client-side bidirectional stream on the control ALPN over that
/// same connection is real additional work tracked separately (the iOS
/// control-channel transport, see `holoiroh/README.md`'s "Remote kill-switch"
/// section). Until then this returns [`HOLOIROH_ERR_UNSUPPORTED`] rather than
/// pretending to send -- it never panics.
///
/// # Safety
/// `bridge` may be a live pointer or null; `json_cstr`/`out_error` follow the
/// same contract as elsewhere in this module. (Arguments are unused in this
/// build beyond the error reporting.)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_control_send(
    bridge: *mut HoloirohBridge,
    json_cstr: *const c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    let _ = (bridge, json_cstr);
    unsafe {
        set_error(
            out_error,
            "control channel not implemented in this build: the holoiroh/control/1 ALPN transport \
             is separate follow-on work (see ios-bridge module doc and PROTOCOL.md)",
        );
    }
    HOLOIROH_ERR_UNSUPPORTED
}

/// Non-blocking poll for the next `ServerMessage` (per `../PROTOCOL.md`)
/// received on the control channel.
///
/// **Not implemented in this build** -- same reason as
/// [`holoiroh_ios_bridge_control_send`]. Sets `*out_json` to null and returns
/// [`HOLOIROH_ERR_UNSUPPORTED`]; never panics.
///
/// # Safety
/// `bridge` may be a live pointer or null; `out_json`, if non-null, must be a
/// valid writable `*mut *mut c_char`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_poll_control_event(
    bridge: *mut HoloirohBridge,
    out_json: *mut *mut c_char,
) -> HoloirohStatus {
    let _ = bridge;
    if !out_json.is_null() {
        unsafe {
            *out_json = std::ptr::null_mut();
        }
    }
    HOLOIROH_ERR_UNSUPPORTED
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
