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
//! # Status: scaffold only
//!
//! Every `extern "C"` function below has a real, stable signature but an
//! `unimplemented!()`/stub body. Wiring these to actual `iroh_live::Live`
//! calls (`Live::from_env`, `Live::subscribe`, `Subscription::media`,
//! `LiveTicket::from_str`, plus the `holoiroh/control/1` ALPN control
//! channel from `../PROTOCOL.md`) is separate follow-on work -- this crate's
//! job here is to pin down the ABI surface so the Swift side and the Rust
//! side can be built independently against a stable contract.
//!
//! # FFI design notes
//!
//! - **Opaque handles, not exposed structs.** Every stateful object
//!   (`Endpoint`, `Subscription`, `ControlChannel`) crosses the boundary as
//!   an opaque `*mut c_void`-style pointer (typed here as a pointer to a
//!   zero-sized marker struct, so C/Swift can't accidentally dereference
//!   it) obtained from a `_new`/`_connect` function and released by a
//!   matching `_free` function. This is the same pattern `iroh-ffi` uses
//!   internally before uniffi's codegen wraps it in a Swift class -- we're
//!   doing by hand what uniffi would otherwise generate.
//! - **No `async fn` across the FFI boundary.** `async`/`.await` doesn't
//!   have a C ABI. Every `extern "C"` function here is synchronous from the
//!   caller's point of view; internally, a `Bridge` owns a Tokio
//!   multi-thread runtime (see `BridgeHandle`) and blocks the calling
//!   thread on `runtime.block_on(...)` for connect/subscribe calls, or
//!   returns immediately for polling calls (`poll_next_frame`,
//!   `poll_control_event`) which check a channel without blocking. Swift
//!   is expected to call the polling functions from a background
//!   `DispatchQueue`/`Task`, not the main thread.
//! - **Error reporting via out-param + sentinel return, not panics.**
//!   `extern "C"` functions must never unwind across the FFI boundary
//!   (undefined behavior in Rust; a hard crash in practice). Every
//!   fallible function returns a sentinel (null pointer / negative int /
//!   `false`) on failure and writes a heap-allocated, null-terminated
//!   error string to an optional `*mut *mut c_char` out-param, freed by
//!   the caller via `holoiroh_ios_bridge_free_error_string`. This mirrors
//!   what uniffi generates automatically (`Result<T, IrohError>` ->
//!   Swift `throws`); we do it by hand here.
//! - **Frames are caller-allocated-and-filled, not Rust-allocated-and-
//!   returned**, to avoid an allocation per frame on a hot path (screen
//!   capture at 30-60fps): `poll_next_frame` takes a caller-owned buffer +
//!   capacity and returns the bytes actually written (or the required
//!   capacity, negated, if the buffer was too small -- the standard
//!   "query then fill" C pattern), rather than returning an owned
//!   allocation the caller would have to remember to free per-frame.
//!
//! # Packaging this crate into an iOS `.xcframework`
//!
//! This is the sequence `iroh-ffi`'s own `cargo make swift-xcframework`
//! target runs (confirmed via `../ios/IROH_FFI.md`'s research), adapted
//! here since this crate has no `uniffi-bindgen` step (its header is
//! hand-generated from `extern "C"` fns via `cbindgen`, not derived from
//! `#[uniffi::export]` macros):
//!
//! 1. **Add the Rust iOS targets** (one-time, per machine -- NOT available
//!    in the environment this scaffold was written in; see
//!    `../ios/IROH_FFI.md`'s "Environment note" section):
//!    ```sh
//!    rustup target add aarch64-apple-ios        # physical device
//!    rustup target add aarch64-apple-ios-sim    # simulator, Apple Silicon
//!    rustup target add x86_64-apple-ios-sim     # simulator, Intel Mac
//!    ```
//! 2. **Build a static lib per target.** Either `cargo build --target
//!    <triple> --release` three times, or use
//!    [`cargo-lipo`](https://crates.io/crates/cargo-lipo) /
//!    [`cargo-xcodebuild`](https://crates.io/crates/cargo-xcodebuild) to
//!    drive this and the `lipo`/`xcodebuild` steps below in one command.
//!    Each produces `target/<triple>/release/libholoiroh_ios_bridge.a`.
//! 3. **Fuse the two simulator slices into one fat binary** (device and
//!    simulator xcframework slices must each be a single binary, but
//!    "simulator" needs to cover both arm64 Macs and Intel Macs):
//!    ```sh
//!    lipo -create \
//!      target/aarch64-apple-ios-sim/release/libholoiroh_ios_bridge.a \
//!      target/x86_64-apple-ios-sim/release/libholoiroh_ios_bridge.a \
//!      -output libholoiroh_ios_bridge-sim.a
//!    ```
//! 4. **Generate the C header** from this file's `extern "C"` signatures:
//!    ```sh
//!    cbindgen --config cbindgen.toml --crate holoiroh-ios-bridge \
//!      --output HoloirohIosBridge.h
//!    ```
//!    (`cbindgen.toml` not yet added -- default config is a reasonable
//!    starting point; see `iroh-ffi`'s use of a hand-maintained header for
//!    a UniFFI crate as a design precedent, though here the header is the
//!    *primary* source of the Swift-visible surface, not a generated
//!    afterthought.)
//! 5. **Assemble the xcframework** from the device slice + fused simulator
//!    slice, each paired with the same header:
//!    ```sh
//!    xcodebuild -create-xcframework \
//!      -library target/aarch64-apple-ios/release/libholoiroh_ios_bridge.a \
//!      -headers include/ \
//!      -library libholoiroh_ios_bridge-sim.a \
//!      -headers include/ \
//!      -output HoloirohIosBridge.xcframework
//!    ```
//! 6. **Wrap it for Swift.** A `module.modulemap` inside the xcframework
//!    (or alongside it, referenced from a small SwiftPM `Package.swift`
//!    binary target, the same shape `iroh-ffi`'s own `Package.swift`
//!    uses for its `Iroh` binary target) exposes the C symbols as an
//!    importable module:
//!    ```text
//!    module HoloirohIosBridge {
//!        header "HoloirohIosBridge.h"
//!        export *
//!    }
//!    ```
//!    Then `import HoloirohIosBridge` works from Swift, and a thin
//!    hand-written Swift wrapper class (not yet written -- separate
//!    follow-on work once the Rust implementations are real) gives the
//!    app an ergonomic API instead of raw C function calls.

use std::ffi::{c_char, c_int};

/// Opaque handle to a running bridge instance: owns the Tokio runtime, the
/// `iroh_live::Live` session once connected, and the current subscription
/// (if any). Obtained via [`holoiroh_ios_bridge_new`], released via
/// [`holoiroh_ios_bridge_free`].
///
/// Zero-sized and never constructed on the Rust side as a real value --
/// only ever handed out as `Box::into_raw(Box::new(BridgeInner {..}))` cast
/// to this opaque type, so C/Swift can hold and pass the pointer around
/// without any ability to read its layout (the real state lives in a
/// private, non-`#[repr(C)]` Rust struct behind it). (Deliberately not a
/// bare `*mut c_void`: giving `HoloirohBridge` and `HoloirohSubscription`
/// distinct named types means Swift/C callers get a type error if they
/// pass the wrong handle to the wrong function, which `c_void` erasure
/// would silently allow.)
#[repr(C)]
pub struct HoloirohBridge {
    _private: [u8; 0],
}

/// Opaque handle to an active broadcast subscription (post-`subscribe`),
/// distinct from [`HoloirohBridge`] so a bridge could in principle hold
/// zero or one active subscriptions independently of its connection
/// lifecycle. Obtained via [`holoiroh_ios_bridge_subscribe`], released via
/// [`holoiroh_ios_bridge_subscription_free`].
#[repr(C)]
pub struct HoloirohSubscription {
    _private: [u8; 0],
}

/// One decoded media frame handed back across the FFI boundary by
/// [`holoiroh_ios_bridge_poll_next_frame`]. `data`/`data_len` describe a
/// caller-allocated buffer the Rust side fills in-place (see module doc,
/// "Frames are caller-allocated-and-filled"); `width`/`height` and
/// `timestamp_us` are metadata the Swift side needs to hand the bytes to
/// `CVPixelBuffer`/`AVSampleBufferDisplayLayer` correctly.
#[repr(C)]
pub struct HoloirohFrame {
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp, microseconds since the subscription started.
    pub timestamp_us: u64,
    /// 0 = video, 1 = audio. Kept as a plain tag rather than a Rust `enum`
    /// crossing FFI, since `#[repr(C)]` enums are still awkward to bind
    /// safely from Swift compared to a plain integer + doc comment.
    pub kind: u8,
}

/// Result/error-code convention shared by every fallible function in this
/// module: `0` = success, negative = failure. Specific negative values are
/// not yet finalized (this is a stub crate -- see module doc) but the sign
/// convention is fixed now so the Swift wrapper's error-handling shape
/// doesn't need to change once real error variants are added.
pub type HoloirohStatus = c_int;

pub const HOLOIROH_OK: HoloirohStatus = 0;
pub const HOLOIROH_ERR_UNKNOWN: HoloirohStatus = -1;
pub const HOLOIROH_ERR_INVALID_TICKET: HoloirohStatus = -2;
pub const HOLOIROH_ERR_CONNECT_FAILED: HoloirohStatus = -3;
pub const HOLOIROH_ERR_NOT_CONNECTED: HoloirohStatus = -4;
pub const HOLOIROH_ERR_BUFFER_TOO_SMALL: HoloirohStatus = -5;

// ---------------------------------------------------------------------
// Lifecycle: bridge construction / teardown
// ---------------------------------------------------------------------

/// Creates a new bridge instance: spins up an internal Tokio multi-thread
/// runtime but does **not** connect yet (see
/// [`holoiroh_ios_bridge_ticket_connect`]). Returns null on failure (e.g.
/// runtime construction failed -- extremely unlikely, but `extern "C"`
/// functions must never panic, see module doc).
///
/// # Safety
/// The returned pointer, if non-null, must eventually be passed to exactly
/// one call of [`holoiroh_ios_bridge_free`]. It must not be dereferenced
/// directly by the caller (opaque type) and must not be used after being
/// freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_new() -> *mut HoloirohBridge {
    // STUB: real implementation constructs a Tokio runtime
    // (tokio::runtime::Builder::new_multi_thread()...build()) and
    // Box::into_raw(Box::new(...)) it behind this opaque pointer.
    unimplemented!(
        "holoiroh_ios_bridge_new: scaffold only, see ios-bridge crate module doc -- \
         real impl builds a Tokio runtime and boxes it behind the opaque handle"
    )
}

/// Releases a bridge instance created by [`holoiroh_ios_bridge_new`],
/// tearing down its Tokio runtime and any active `iroh_live::Live`
/// session. Passing null is a no-op (matches `free(NULL)` C convention).
///
/// # Safety
/// `bridge` must either be null or a pointer previously returned by
/// [`holoiroh_ios_bridge_new`] and not already freed. The caller must not
/// use `bridge` again after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_free(bridge: *mut HoloirohBridge) {
    if bridge.is_null() {
        return;
    }
    // STUB: real implementation does `drop(Box::from_raw(bridge as *mut
    // BridgeInner))`, which runs `Live::shutdown().await` via
    // `runtime.block_on` before the runtime itself is dropped.
    unimplemented!(
        "holoiroh_ios_bridge_free: scaffold only, see ios-bridge crate module doc"
    )
}

// ---------------------------------------------------------------------
// Ticket-connect
// ---------------------------------------------------------------------

/// Parses an `iroh-live:` ticket string (the format `LiveTicket::serialize`
/// produces -- see `../ios/IROH_FFI.md`'s Finding (b)) and connects the
/// bridge's `iroh_live::Live` session to the peer it describes. Blocks the
/// calling thread until the connection attempt resolves (success or
/// failure) -- call from a background queue, not the main thread.
///
/// `ticket_cstr` must be a null-terminated UTF-8 C string, e.g. as
/// produced by Swift's `String.withCString`. `out_error` may be null if
/// the caller doesn't want a human-readable error message on failure (the
/// [`HoloirohStatus`] return value alone still distinguishes failure
/// modes).
///
/// Returns [`HOLOIROH_OK`] on success, or a negative
/// [`HoloirohStatus`] (see constants above) on failure.
///
/// # Safety
/// `bridge` must be a live pointer from [`holoiroh_ios_bridge_new`].
/// `ticket_cstr` must be a valid null-terminated C string for the duration
/// of this call. If non-null, `out_error` must be a valid, writable
/// `*mut c_char` slot; any string written there must later be freed via
/// [`holoiroh_ios_bridge_free_error_string`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_ticket_connect(
    bridge: *mut HoloirohBridge,
    ticket_cstr: *const c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    // STUB: real implementation:
    //   1. CStr::from_ptr(ticket_cstr).to_str() -> ticket string
    //   2. iroh_live::LiveTicket::from_str(ticket) -> LiveTicket
    //      (HOLOIROH_ERR_INVALID_TICKET on parse failure)
    //   3. runtime.block_on(live.subscribe(ticket.endpoint, &ticket.broadcast_name))
    //      -- see Live::subscribe in iroh-live/src/live.rs
    //      (HOLOIROH_ERR_CONNECT_FAILED on failure)
    //   4. store the resulting Subscription on the BridgeInner
    let _ = (bridge, ticket_cstr, out_error);
    unimplemented!(
        "holoiroh_ios_bridge_ticket_connect: scaffold only, see ios-bridge crate module doc -- \
         real impl parses an iroh-live: ticket and calls Live::subscribe"
    )
}

// ---------------------------------------------------------------------
// Subscribe (media broadcast)
// ---------------------------------------------------------------------

/// Begins consuming the media broadcast on an already-connected bridge
/// (see [`holoiroh_ios_bridge_ticket_connect`]), returning an opaque
/// subscription handle that [`holoiroh_ios_bridge_poll_next_frame`] reads
/// from. Corresponds to `iroh_live::Subscription::media()` /
/// `Live::subscribe_media` in the underlying Rust API (see
/// `../ios/IROH_FFI.md`'s Finding (b) for the exact upstream signatures).
///
/// Returns null on failure (bridge not connected yet, or the broadcast
/// has no matching media tracks); check `out_error` if non-null.
///
/// # Safety
/// `bridge` must be a live, connected pointer (post successful
/// [`holoiroh_ios_bridge_ticket_connect`]). `out_error` follows the same
/// contract as in [`holoiroh_ios_bridge_ticket_connect`]. The returned
/// pointer, if non-null, must eventually be passed to exactly one call of
/// [`holoiroh_ios_bridge_subscription_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_subscribe(
    bridge: *mut HoloirohBridge,
    out_error: *mut *mut c_char,
) -> *mut HoloirohSubscription {
    // STUB: real implementation calls
    // runtime.block_on(subscription.media_with_decoders::<DefaultDecoders>(...))
    // (see iroh-live/src/subscription.rs `Subscription::media`) and boxes
    // the resulting MediaTracks handle behind this opaque pointer.
    let _ = (bridge, out_error);
    unimplemented!(
        "holoiroh_ios_bridge_subscribe: scaffold only, see ios-bridge crate module doc -- \
         real impl calls Subscription::media_with_decoders"
    )
}

/// Releases a subscription created by [`holoiroh_ios_bridge_subscribe`].
/// Passing null is a no-op.
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
    unimplemented!(
        "holoiroh_ios_bridge_subscription_free: scaffold only, see ios-bridge crate module doc"
    )
}

// ---------------------------------------------------------------------
// Poll next frame
// ---------------------------------------------------------------------

/// Non-blocking poll for the next decoded frame on an active subscription.
/// Fills `out_frame` (metadata) and copies up to `buf_capacity` bytes of
/// frame data into caller-owned `buf`, returning the number of bytes
/// actually written into `buf` on success.
///
/// If no frame is available yet, returns `0` with `out_frame` left
/// untouched -- **not** an error; the caller should poll again shortly
/// (e.g. on a `CADisplayLink`-driven or timer-driven loop on a background
/// queue). If a frame is available but `buf_capacity` is smaller than the
/// frame's actual byte length, returns [`HOLOIROH_ERR_BUFFER_TOO_SMALL`]
/// (negative) and writes the *required* capacity into `out_frame.width`/
/// `.height`... no -- see note below; this is the "query then fill"
/// pattern referenced in the module doc, exact required-size reporting
/// mechanism to be finalized alongside the real implementation.
///
/// Returns a non-negative byte count on success (frame written, possibly
/// `0` meaning "no frame yet"), or a negative [`HoloirohStatus`] on
/// failure.
///
/// # Safety
/// `subscription` must be a live pointer from
/// [`holoiroh_ios_bridge_subscribe`]. `buf` must be valid and writable for
/// `buf_capacity` bytes. `out_frame` must be a valid, writable
/// `*mut HoloirohFrame`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_poll_next_frame(
    subscription: *mut HoloirohSubscription,
    buf: *mut u8,
    buf_capacity: usize,
    out_frame: *mut HoloirohFrame,
) -> c_int {
    // STUB: real implementation does a non-blocking
    // `try_recv`/`now_or_never` on the MediaTracks' frame channel (backed
    // by the Tokio runtime owned by the parent BridgeInner -- subscription
    // handles need a way back to that runtime, likely via a shared Arc
    // stored alongside the opaque pointer's real Rust struct), copies the
    // frame's bytes into `buf` if it fits, and fills `out_frame`.
    let _ = (subscription, buf, buf_capacity, out_frame);
    unimplemented!(
        "holoiroh_ios_bridge_poll_next_frame: scaffold only, see ios-bridge crate module doc -- \
         real impl does a non-blocking recv on the decoded-frame channel"
    )
}

// ---------------------------------------------------------------------
// Control channel (PROTOCOL.md ClientMessage / ServerMessage)
// ---------------------------------------------------------------------

/// Sends one `ClientMessage` (per `../PROTOCOL.md`) over the control
/// channel to the connected Mac daemon. `json_cstr` must be a
/// null-terminated UTF-8 C string containing one JSON object matching
/// `PROTOCOL.md`'s `ClientMessage` schema (e.g.
/// `{"type":"prompt","text":"..."}`) -- serialization is the Swift side's
/// job; this function just frames and sends the already-serialized line.
///
/// # Safety
/// `bridge` must be a live, connected pointer. `json_cstr` must be a valid
/// null-terminated C string for the duration of this call. `out_error`
/// follows the same contract as elsewhere in this module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_control_send(
    bridge: *mut HoloirohBridge,
    json_cstr: *const c_char,
    out_error: *mut *mut c_char,
) -> HoloirohStatus {
    // STUB: real implementation writes `json_cstr` + "\n" to the
    // ControlChannel's SendStream (see mac-daemon/src/control_channel.rs
    // and PROTOCOL.md's NDJSON framing -- the Swift side is the dial side
    // per PROTOCOL.md's "Direction" section, so this opens/reuses a bi
    // stream via Connection::open_bi()).
    let _ = (bridge, json_cstr, out_error);
    unimplemented!(
        "holoiroh_ios_bridge_control_send: scaffold only, see ios-bridge crate module doc -- \
         real impl writes NDJSON per PROTOCOL.md over the control-channel bi stream"
    )
}

/// Non-blocking poll for the next `ServerMessage` (per `../PROTOCOL.md`)
/// received on the control channel. On success, writes a
/// heap-allocated, null-terminated JSON string (the raw `ServerMessage`
/// line) to `*out_json` and returns [`HOLOIROH_OK`]; the caller must free
/// it via [`holoiroh_ios_bridge_free_error_string`] (reused here as a
/// generic "free a Rust-allocated C string" function -- see its doc).
/// If no message is available yet, returns [`HOLOIROH_OK`] with
/// `*out_json` set to null -- **not** an error, same "poll again later"
/// convention as [`holoiroh_ios_bridge_poll_next_frame`].
///
/// # Safety
/// `bridge` must be a live, connected pointer. `out_json` must be a
/// valid, writable `*mut *mut c_char`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_poll_control_event(
    bridge: *mut HoloirohBridge,
    out_json: *mut *mut c_char,
) -> HoloirohStatus {
    // STUB: real implementation does a non-blocking try_recv on a channel
    // fed by a background task reading ServerMessage lines off the
    // control channel's RecvStream (mirroring
    // mac-daemon/src/control_channel.rs's read loop on the daemon side).
    let _ = (bridge, out_json);
    unimplemented!(
        "holoiroh_ios_bridge_poll_control_event: scaffold only, see ios-bridge crate module doc"
    )
}

// ---------------------------------------------------------------------
// Shared teardown helper
// ---------------------------------------------------------------------

/// Frees a C string previously allocated by this crate and handed back
/// through an `out_error`/`out_json` out-parameter (e.g. from
/// [`holoiroh_ios_bridge_ticket_connect`] or
/// [`holoiroh_ios_bridge_poll_control_event`]). Passing null is a no-op.
///
/// Every Rust-allocated string crossing this FFI boundary **must** be
/// freed via this function, never via Swift's own memory management or
/// libc `free` directly -- the allocator that created it (Rust's global
/// allocator, via `CString::into_raw`) must also be the one that
/// deallocates it.
///
/// # Safety
/// `s` must either be null or a pointer previously returned in an
/// `out_error`/`out_json` parameter by a function in this crate, and must
/// not have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn holoiroh_ios_bridge_free_error_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // STUB (though this one is close to real): real implementation is
    // `drop(CString::from_raw(s))`. Left as unimplemented! for now purely
    // for consistency with every other function in this scaffold (so
    // nothing here silently "half-works" while everything else is a
    // stub) -- trivial to make real the moment the allocating side is.
    unimplemented!(
        "holoiroh_ios_bridge_free_error_string: scaffold only -- real impl is \
         drop(CString::from_raw(s))"
    )
}
