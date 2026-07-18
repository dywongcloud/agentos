//! macOS permission preflight checks.
//!
//! Broadcasting the Mac's screen requires the Screen Recording TCC
//! permission, and driving the Mac via `holo-desktop-cli` (mouse/keyboard
//! synthetic events) requires the Accessibility permission. Both are
//! per-app grants the user makes in System Settings; there is no way to
//! request-and-block until granted for Screen Recording (macOS shows the
//! prompt once, then the user must go grant it manually), so the correct
//! behavior is: check both up front, and if either is missing, tell the
//! user exactly what to do and refuse to start the broadcast rather than
//! producing a black/frozen stream or a daemon that can't actually drive
//! the Mac.
//!
//! Bindings come from `objc2-core-graphics` (`CGPreflightScreenCaptureAccess`)
//! and `objc2-application-services` (`AXIsProcessTrusted`) -- both are
//! already part of this crate's dependency graph (the former transitively,
//! via `iroh-live`'s macOS capture backend; the latter added directly for
//! this check), so no raw FFI `extern "C"` declarations are needed.

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

/// Result of the full preflight: every permission that is currently
/// missing. Empty means the broadcast is clear to start.
#[derive(Debug, Default)]
pub struct PreflightResult {
    pub missing: Vec<MissingPermission>,
}

impl PreflightResult {
    pub fn is_ok(&self) -> bool {
        self.missing.is_empty()
    }

    /// Print one clear instruction block per missing permission. Called
    /// for *all* missing permissions at once (never short-circuits on the
    /// first one), so the user sees the full list of what to fix in one
    /// pass instead of discovering them one restart at a time.
    pub fn report(&self) {
        for permission in &self.missing {
            eprintln!("[holoiroh-daemon] Missing permission: {permission}");
            eprintln!("  {}", permission.instruction());
        }
    }
}

/// Run both macOS permission preflight checks. Never panics: both
/// underlying APIs are simple synchronous TCC queries that return a bool,
/// no allocation or Objective-C exception path to guard against.
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

/// `CGPreflightScreenCaptureAccess()` -- returns whether this process
/// currently has Screen Recording access, without prompting the user.
/// macOS-only; the crate as a whole only builds for macOS (see
/// `holoiroh-daemon`'s use of `ScreenCaptureKit` elsewhere in the
/// broadcast pipeline), but this function is still individually gated so
/// it can never be referenced on a non-macOS target.
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
