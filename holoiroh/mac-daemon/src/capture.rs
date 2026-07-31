//! Screen capture wiring: macOS ScreenCaptureKit as the video source for a
//! [`LocalBroadcast`].
//!
//! This module uses `iroh-live`'s `rusty-capture` crate's screen/display capturer
//! (`ScreenCapturer`), re-exported at `iroh_live::media::capture`. This is deliberately the
//! screen side, not `CameraCapturer`: the daemon streams the desktop, never a webcam (see
//! holoiroh/README.md's architecture overview).
//!
//! Reference: `iroh-live-cli`'s own `setup_screen_source`, in `iroh-live-cli/src/source.rs` of
//! n0-computer/iroh-live, commit `5f95758fcd1450e443a9134c9d9342bcc3957b85` (the same commit
//! this workspace's `Cargo.toml` pins). That function follows the identical non-Linux pattern:
//! call `ScreenCapturer::with_monitor(&monitor)` or `ScreenCapturer::new()`, then call
//! `broadcast.video().set_source(screen, codec, presets)`. On macOS the `ScreenCapturer`
//! re-export bottoms out at `MacScreenCapturer`, backed by ScreenCaptureKit
//! (`rusty-capture/src/platform/apple/screen.rs`). The `screen-apple` feature gates this path,
//! and `iroh-live`'s default `capture` feature enables it transitively, through
//! `moq-media/capture-screen` then `rusty-capture/screen-apple`.

use iroh_live::media::{
    capture::{MonitorInfo, ScreenCapturer, ScreenConfig},
    codec::VideoCodec,
    format::VideoPreset,
    publish::LocalBroadcast,
};

/// Enumerates displays available for screen capture via `ScreenCapturer`.
///
/// This function wraps [`ScreenCapturer::list_all`], rather than [`ScreenCapturer::list`], so
/// every compiled-in backend is represented. On macOS with the `screen-apple` feature, this is
/// just `ScreenCaptureKit`. But `list_all` keeps the enumeration honest if this daemon ever
/// compiles in other backends, such as `xcap`, alongside it.
pub fn list_displays() -> anyhow::Result<Vec<MonitorInfo>> {
    ScreenCapturer::list_all()
}

/// Resolves a `--display <index>` CLI argument to a specific [`MonitorInfo`].
///
/// - `Some(index)`: this function looks up that index in the enumerated display list.
///   Out-of-range indices produce a clear error listing what is available, rather than panicking.
/// - `None`: this function defaults to the primary display (`MonitorInfo::is_primary`). It falls
///   back to the first enumerated display if no display reports itself as primary. This fallback
///   is defensive: ScreenCaptureKit is expected to always mark one display as primary, but the
///   fallback keeps this function from hard-failing on an unexpected platform or backend quirk.
///
/// This function returns an error, not a panic, when no displays are enumerated at all. On
/// macOS this almost always means the daemon binary lacks Screen Recording permission, so the
/// error message says so explicitly.
pub fn resolve_display(index: Option<usize>) -> anyhow::Result<MonitorInfo> {
    let displays = list_displays()?;

    if displays.is_empty() {
        anyhow::bail!(
            "no displays available for screen capture. On macOS this usually means the \
             daemon binary has not been granted Screen Recording permission -- check \
             System Settings -> Privacy & Security -> Screen Recording."
        );
    }

    match index {
        Some(idx) => displays.get(idx).cloned().ok_or_else(|| {
            let available: Vec<String> = displays.iter().map(|m| m.summary()).collect();
            anyhow::anyhow!(
                "--display index {idx} out of range ({} available):\n  {}",
                displays.len(),
                available.join("\n  ")
            )
        }),
        None => Ok(displays
            .iter()
            .find(|m| m.is_primary)
            .cloned()
            .unwrap_or_else(|| displays[0].clone())),
    }
}

/// The capture configuration the daemon actually uses, exposed so `video_latency_probe` can
/// measure the real capturer at the real rate rather than a copy of it that could drift.
pub fn screen_config() -> ScreenConfig {
    ScreenConfig {
        target_fps: Some(CAPTURE_FPS),
        ..ScreenConfig::default()
    }
}

/// Opens the resolved display via [`ScreenCapturer::with_monitor_config`], and wires it into
/// `broadcast` as the video source with the given codec and presets.
///
/// This function follows `iroh-live-cli`'s `setup_screen_source` non-Linux branch. It differs in
/// one way: it passes an explicit [`ScreenConfig`], instead of taking the default one. See
/// [`CAPTURE_FPS`] for why the default rate is wrong here. This function has no
/// PipeWire-restore-token branch, since that path is Linux-only and this daemon is macOS-only
/// (see holoiroh/README.md).
pub fn setup_screen_video(
    broadcast: &LocalBroadcast,
    display_index: Option<usize>,
    codec: VideoCodec,
    presets: &[VideoPreset],
) -> anyhow::Result<()> {
    let monitor = resolve_display(display_index)?;
    tracing::info!(
        display = %monitor.summary(),
        "opening ScreenCaptureKit capturer for selected display"
    );

    let screen = ScreenCapturer::with_monitor_config(&monitor, &screen_config())?;
    broadcast
        .video()
        .set_source(screen, codec, presets.to_vec())?;

    Ok(())
}

/// Capture rate. This module deliberately sets the rate above the encoder's own poll rate,
/// rather than equal to it.
///
/// The encoder pipeline wakes once per 33.3ms, and takes whatever the latest captured frame is
/// (`moq-media/src/pipeline/video_encode.rs`). Capturing at that same 30Hz means two independent
/// clocks beat against each other: the poll repeatedly lands just before the next frame, and
/// picks up one a full period old. The phase between the two clocks drifts.
///
/// `video_latency_probe` measures the cost on this machine. This probe holds everything else
/// fixed and varies only the source rate. Capture-to-decoded-frame p50 was 187ms at 30fps, 71ms
/// at 45, 68ms at 60, 67ms at 90, and 65ms at 120. The delivered rate stayed at 28fps in every
/// case, because the encoder loop is the throughput limit either way. So this rate buys about
/// 119ms of video-feedback latency -- the delay that makes a remote cursor feel late -- without
/// encoding a single extra frame. Nearly all of the gain clears at 30; past 60 the gain is noise.
/// So this constant does not go higher, since a higher rate would pay for capture work that buys
/// nothing.
pub const CAPTURE_FPS: f32 = 60.0;
