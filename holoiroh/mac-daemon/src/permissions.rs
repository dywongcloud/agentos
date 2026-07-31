//! macOS permission preflight checks.
//!
//! Broadcasting the Mac's screen requires the Screen Recording TCC
//! permission. Driving the Mac via `holo-desktop-cli` (mouse/keyboard
//! synthetic events) requires the Accessibility permission. Both are
//! per-app grants. The user makes these grants in System Settings.
//!
//! There is no way to request-and-block until the user grants Screen
//! Recording. macOS shows the prompt once. Then the user must go grant it
//! manually. The correct behavior is:
//!
//! - Check both permissions up front.
//! - If either permission is missing, tell the user exactly what to do.
//! - Refuse to start the broadcast.
//!
//! This avoids two problems: a black or frozen stream, and a daemon that
//! cannot actually drive the Mac.
//!
//! Bindings come from `objc2-core-graphics` (`CGPreflightScreenCaptureAccess`)
//! and `objc2-application-services` (`AXIsProcessTrusted`). Both are
//! already part of this crate's dependency graph. The former is included
//! transitively, via `iroh-live`'s macOS capture backend. The latter was
//! added directly for this check. As a result, this module needs no raw
//! FFI `extern "C"` declarations.

use std::fmt;

/// Which macOS permission is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingPermission {
    ScreenRecording,
    Accessibility,
}

impl MissingPermission {
    /// Exact, copy-pasteable instruction for granting this permission.
    pub fn instruction(&self) -> &'static str {
        match self {
            MissingPermission::ScreenRecording => {
                "Screen Recording permission is not granted. Open System \
                 Settings > Privacy & Security > Screen Recording, enable \
                 the toggle for this app (holoiroh-daemon / your terminal \
                 if running via `cargo run`), then restart the daemon. \
                 macOS requires the app to be fully quit and relaunched \
                 after granting this permission -- it will not take effect \
                 on a running process."
            }
            MissingPermission::Accessibility => {
                "Accessibility permission is not granted. Open System \
                 Settings > Privacy & Security > Accessibility, enable the \
                 toggle for this app (holoiroh-daemon / your terminal if \
                 running via `cargo run`), then restart the daemon."
            }
        }
    }
}

impl fmt::Display for MissingPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MissingPermission::ScreenRecording => write!(f, "Screen Recording"),
            MissingPermission::Accessibility => write!(f, "Accessibility"),
        }
    }
}

/// This is the result of the full preflight check. It lists every
/// permission that is currently missing. An empty list means the
/// broadcast is clear to start.
#[derive(Debug, Default)]
pub struct PreflightResult {
    pub missing: Vec<MissingPermission>,
}

impl PreflightResult {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty()
    }

    /// Print one clear instruction block per missing permission. This
    /// function processes *all* missing permissions at once. It never
    /// stops after the first one. As a result, the user sees the full
    /// list of what to fix in one pass, instead of discovering problems
    /// one restart at a time.
    pub fn report(&self) {
        for permission in &self.missing {
            eprintln!("[holoiroh-daemon] Missing permission: {permission}");
            eprintln!("  {}", permission.instruction());
        }
    }
}

/// Run both macOS permission preflight checks. This function never
/// panics. Both underlying APIs are simple, synchronous TCC queries that
/// return a bool. There is no allocation and no Objective-C exception
/// path to guard against.
pub fn preflight() -> PreflightResult {
    let mut missing = Vec::new();

    if !screen_recording_granted() {
        missing.push(MissingPermission::ScreenRecording);
    }
    if !accessibility_granted() {
        missing.push(MissingPermission::Accessibility);
    }

    PreflightResult { missing }
}

/// `CGPreflightScreenCaptureAccess()` returns whether this process
/// currently has Screen Recording access. It does not prompt the user.
///
/// This function is macOS-only. The crate as a whole only builds for
/// macOS. See `holoiroh-daemon`'s use of `ScreenCaptureKit` elsewhere in
/// the broadcast pipeline. Even so, this function is still individually
/// gated. As a result, it can never be referenced on a non-macOS target.
#[cfg(target_os = "macos")]
pub fn screen_recording_granted() -> bool {
    // objc2-core-graphics binds this as a safe fn (no `unsafe extern` in its
    // generated signature) -- it takes no arguments, returns a plain bool,
    // and performs no side effect beyond querying TCC state (does not
    // prompt the user). Callable from any thread per Apple's docs.
    objc2_core_graphics::CGPreflightScreenCaptureAccess()
}

#[cfg(not(target_os = "macos"))]
pub fn screen_recording_granted() -> bool {
    compile_error!("holoiroh-daemon is macOS-only; screen_recording_granted() has no non-macOS implementation");
}

/// `AXIsProcessTrusted()` -- returns whether this process is trusted for
/// Accessibility (assistive) access, without prompting the user.
#[cfg(target_os = "macos")]
pub fn accessibility_granted() -> bool {
    // Safety: AXIsProcessTrusted takes no arguments and returns a plain
    // bool; it only queries current trust state and does not prompt.
    unsafe { objc2_application_services::AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_granted() -> bool {
    compile_error!("holoiroh-daemon is macOS-only; accessibility_granted() has no non-macOS implementation");
}

#[cfg(target_os = "macos")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// `Boolean IsSecureEventInputEnabled(void)` from `<Carbon/HIToolbox/Events.h>`.
    ///
    /// This function is exported by `Carbon.framework`, the umbrella
    /// framework `HIToolbox` lives under. It is NOT exported by
    /// `ApplicationServices.framework`. This was live-witnessed: linking
    /// against `ApplicationServices` alone left `_IsSecureEventInputEnabled`
    /// undefined at link time. This happened despite
    /// `ApplicationServices.framework` being already linked transitively
    /// via `objc2-application-services`.
    ///
    /// This is a public, documented, long-stable Apple API.
    /// `objc2-application-services` does not bind it. This is why the code
    /// declares it directly here, instead of using a generated binding.
    ///
    /// `Boolean` is a C `unsigned char` (`0`/`1`). This is not the same ABI
    /// as Rust `bool`. So the raw return type is `u8`.
    fn IsSecureEventInputEnabled() -> u8;
}

/// Whether the CURRENTLY FOCUSED field anywhere on the system is a secure
/// (password-class) input field. This is true whenever one of these has
/// focus:
///
/// - The loginwindow's authentication UI.
/// - A screen-lock password prompt.
/// - A `sudo`/Keychain dialog.
/// - Any other app's explicit `EnableSecureEventInput()` call.
///
/// This is macOS's own signal for "a screen-capture/keystroke-
/// synthesis-hostile field is active right now". This is the exact
/// condition behind the live-reported black rectangle where the login
/// window's password field should render.
///
/// ScreenCaptureKit deliberately excludes secure UI from every captured
/// frame while this is true. This is a WindowServer-level security
/// boundary. No process can bypass it, regardless of privilege.
///
/// This is a read-only query with no side effects. It is safe to poll on
/// any thread, at any cadence.
#[cfg(target_os = "macos")]
pub fn secure_input_active() -> bool {
    // Safety: IsSecureEventInputEnabled takes no arguments and returns a
    // plain Boolean; it only queries current secure-input state and never
    // prompts, blocks, or mutates anything.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

#[cfg(not(target_os = "macos"))]
pub fn secure_input_active() -> bool {
    compile_error!("holoiroh-daemon is macOS-only; secure_input_active() has no non-macOS implementation");
}
