//! Screen capture wiring: macOS ScreenCaptureKit as the video source for a
//! [`LocalBroadcast`].
//!
//! Uses `iroh-live`'s `rusty-capture` crate's **screen/display** capturer
//! (`ScreenCapturer`), re-exported at `iroh_live::media::capture`. This is
//! deliberately the screen side, not `CameraCapturer` -- the daemon streams
//! the desktop, never a webcam (see holoiroh/README.md's architecture
//! overview).
//!
//! Reference: `iroh-live-cli`'s own `setup_screen_source` (in
//! `iroh-live-cli/src/source.rs` of n0-computer/iroh-live, commit
//! `5f95758fcd1450e443a9134c9d9342bcc3957b85` -- the same commit this
//! workspace's `Cargo.toml` pins) follows the identical non-Linux pattern:
//! `ScreenCapturer::with_monitor(&monitor)` or `ScreenCapturer::new()`, then
//! `broadcast.video().set_source(screen, codec, presets)`. On macOS the
//! `ScreenCapturer` re-export bottoms out at `MacScreenCapturer`, backed by
//! ScreenCaptureKit (`rusty-capture/src/platform/apple/screen.rs`, gated by
//! the `screen-apple` feature -- enabled transitively via `iroh-live`'s
//! default `capture` feature -> `moq-media/capture-screen` ->
//! `rusty-capture/screen-apple`).

use iroh_live::media::{
    capture::{MonitorInfo, ScreenCapturer},
    codec::VideoCodec,
    format::VideoPreset,
    publish::LocalBroadcast,
};

/// Enumerates displays available for screen capture via `ScreenCapturer`.
///
/// Wraps [`ScreenCapturer::list_all`] (rather than [`ScreenCapturer::list`])
/// so every compiled-in backend is represented -- on macOS with the
/// `screen-apple` feature this is just `ScreenCaptureKit`, but `list_all`
/// keeps the enumeration honest if other backends (e.g. `xcap`) are ever
/// compiled in alongside it.
pub fn list_displays() -> anyhow::Result<Vec<MonitorInfo>> {
    ScreenCapturer::list_all()
}

/// Resolves a `--display <index>` CLI argument to a specific [`MonitorInfo`].
///
/// - `Some(index)`: looks up that index in the enumerated display list.
///   Out-of-range indices produce a clear error listing what *is* available,
///   rather than panicking.
/// - `None`: defaults to the primary display (`MonitorInfo::is_primary`),
///   falling back to the first enumerated display if no display reports
///   itself as primary (defensive -- ScreenCaptureKit is expected to always
///   mark one, but the fallback keeps this from hard-failing on an
///   unexpected platform/backend quirk).
///
/// Returns an error (not a panic) when no displays are enumerated at all --
/// on macOS this is almost always a missing Screen Recording permission
/// grant for the daemon binary, so the error message says so explicitly.
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

/// Opens the resolved display via [`ScreenCapturer::with_monitor`] and wires
/// it into `broadcast` as the video source with the given codec/presets.
///
/// Mirrors `iroh-live-cli`'s `setup_screen_source` non-Linux branch exactly:
/// `ScreenCapturer::with_monitor(&monitor)` then
/// `broadcast.video().set_source(screen, codec, presets)`. There is no
/// PipeWire-restore-token branch here since that path is Linux-only and this
/// daemon is macOS-only (see holoiroh/README.md).
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

    let screen = ScreenCapturer::with_monitor(&monitor)?;
    broadcast
        .video()
        .set_source(screen, codec, presets.to_vec())?;

    Ok(())
}
