//! macOS screen capture via ScreenCaptureKit (macOS 12.3+).
//!
//! Produces IOSurface-backed `CVPixelBuffer` frames wrapped in [`AppleGpuFrame`].
//! The pixel data stays in GPU memory — no CPU copy. VideoToolbox encoder can
//! pass the CVPixelBuffer directly to `VTCompressionSessionEncodeFrame`; wgpu
//! renderer can import via `CVMetalTextureCache`.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rusty_codecs::{
    format::{
        AppleGpuFrame, GpuFrame, GpuPixelFormat, PixelFormat as RcPixelFormat, VideoFormat,
        VideoFrame,
    },
    traits::VideoSource,
};
use screencapturekit::{
    cm::{CMSampleBuffer, CMTime},
    shareable_content::SCShareableContent,
    stream::{
        SCStream,
        configuration::{PixelFormat, SCStreamConfiguration},
        content_filter::SCContentFilter,
        output_trait::SCStreamOutputTrait,
        output_type::SCStreamOutputType,
    },
};
use tracing::{info, warn};

use crate::types::{MonitorInfo, ScreenConfig};

// CoreGraphics APIs for permission check.
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

/// Warns if Screen Recording permission has not been granted.
fn check_screen_capture_permission() {
    let granted = unsafe { CGPreflightScreenCaptureAccess() };
    if !granted {
        warn!(
            "Screen Recording permission not granted. \
             Grant access in System Settings > Privacy & Security > Screen Recording"
        );
    }
}

/// Derive scale factor from SCK display: pixel width / frame width.
/// Returns 2.0 on Retina, 1.0 on non-Retina.
fn display_scale_factor(display: &screencapturekit::shareable_content::SCDisplay) -> f64 {
    // SCDisplay.width() returns logical points — same as frame().width —
    // so we can't derive Retina scale from it. Query NSScreen instead.
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;
    use objc2_foundation::NSString;

    let Some(mtm) = MainThreadMarker::new() else {
        // Not on the main thread — fall back to safe Retina default.
        return 2.0;
    };

    let display_id = display.display_id();

    // Find the NSScreen matching this display via NSScreenNumber in deviceDescription.
    let scale = NSScreen::screens(mtm)
        .iter()
        .find(|screen| {
            let desc = screen.deviceDescription();
            let key = NSString::from_str("NSScreenNumber");
            desc.objectForKey(&key)
                .and_then(|val| {
                    // NSScreenNumber is an NSNumber containing the CGDirectDisplayID.
                    let num: *const objc2::runtime::AnyObject = objc2::rc::Retained::as_ptr(&val);
                    let id: u32 = unsafe { objc2::msg_send![num, unsignedIntValue] };
                    Some(id)
                })
                .is_some_and(|id| id == display_id)
        })
        .map(|screen| screen.backingScaleFactor())
        .unwrap_or(2.0);

    tracing::debug!(display_id, scale, "display scale factor from NSScreen");
    scale
}

/// Lists available macOS displays.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    check_screen_capture_permission();
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get shareable content: {e:?}"))?;
    let displays = content.displays();
    let mut result = Vec::new();
    for (i, display) in displays.iter().enumerate() {
        let width = display.width();
        let height = display.height();
        let frame = display.frame();
        result.push(MonitorInfo {
            backend: crate::CaptureBackend::ScreenCaptureKit,
            id: format!("macos-display-{}", display.display_id()),
            name: format!("Display {}", display.display_id()),
            position: [frame.x as i32, frame.y as i32],
            dimensions: [width, height],
            scale_factor: display_scale_factor(display) as f32,
            refresh_rate_hz: None,
            is_primary: i == 0,
        });
    }
    Ok(result)
}

/// Lists on-screen windows available for capture.
pub fn windows() -> Result<Vec<crate::types::WindowInfo>> {
    check_screen_capture_permission();
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get shareable content: {e:?}"))?;
    // Build display bounds for window→display mapping.
    let displays = content.displays();
    let display_bounds: Vec<_> = displays
        .iter()
        .map(|d| {
            let f = d.frame();
            (d.display_id(), f.x, f.y, f.x + f.width, f.y + f.height)
        })
        .collect();

    let mut result = Vec::new();
    for window in content.windows() {
        let frame = window.frame();
        // Skip tiny windows (status bar items, invisible helpers, menu extras)
        if frame.width < 100.0 || frame.height < 100.0 {
            continue;
        }
        let app_name = window
            .owning_application()
            .map(|app| app.application_name())
            .unwrap_or_default();
        let title = window.title().unwrap_or_default();
        let display_title = if title.is_empty() {
            app_name.clone()
        } else {
            title
        };
        if display_title.is_empty() {
            continue;
        }
        // Find which display contains the window center.
        let cx = frame.x + frame.width / 2.0;
        let cy = frame.y + frame.height / 2.0;
        let display_id = display_bounds
            .iter()
            .find(|(_, x0, y0, x1, y1)| cx >= *x0 && cx < *x1 && cy >= *y0 && cy < *y1)
            .map(|(id, ..)| *id);
        result.push(crate::types::WindowInfo {
            backend: crate::CaptureBackend::ScreenCaptureKit,
            id: window.window_id(),
            title: display_title,
            app_name,
            dimensions: [frame.width as u32, frame.height as u32],
            display_id,
            is_on_screen: window.is_on_screen(),
        });
    }
    Ok(result)
}

/// Actual pixel dimensions observed from the CVPixelBuffer callback.
/// SCK may deliver at a different resolution than requested (e.g. Retina scaling).
struct ActualDimensions {
    width: AtomicU32,
    height: AtomicU32,
}

struct FrameHandler {
    tx: mpsc::SyncSender<VideoFrame>,
    capture_start: Instant,
    actual_dims: Arc<ActualDimensions>,
    /// Milliseconds-since-`capture_start` at which the most recent frame was
    /// delivered. This is the capture stream's liveness heartbeat: ScreenCaptureKit
    /// delivers continuously (~27fps, max observed inter-frame gap 226ms) even on a
    /// completely static screen — measured by `examples/frame_arrival_pattern.rs` —
    /// so a long gap unambiguously means the stream is dead, not that nothing moved.
    last_frame_ms: Arc<AtomicU64>,
}

impl SCStreamOutputTrait for FrameHandler {
    fn did_output_sample_buffer(
        &self,
        sample_buffer: CMSampleBuffer,
        _of_type: SCStreamOutputType,
    ) {
        let Some(pixel_buffer) = sample_buffer.image_buffer() else {
            return;
        };

        let width = pixel_buffer.width() as u32;
        let height = pixel_buffer.height() as u32;

        // Update actual dimensions on first frame (or if source resizes).
        self.actual_dims.width.store(width, Ordering::Relaxed);
        self.actual_dims.height.store(height, Ordering::Relaxed);

        // Zero-copy: retain the CVPixelBuffer and wrap it as a GPU frame.
        let raw = pixel_buffer.as_ptr();
        let gpu_frame =
            unsafe { AppleGpuFrame::from_raw(raw, width, height, GpuPixelFormat::Bgra) };
        let elapsed = self.capture_start.elapsed();
        self.last_frame_ms
            .store(elapsed.as_millis() as u64, Ordering::Relaxed);
        let frame = VideoFrame::new_gpu(GpuFrame::new(Arc::new(gpu_frame)), elapsed);

        // Drop frame if channel is full — backpressure from the callback
        // thread. The consumer drains to latest anyway.
        let _ = self.tx.try_send(frame);
    }
}

/// How this capturer's target was originally selected — retained so a dead
/// capture stream can be torn down and rebuilt against the identical target
/// once it becomes capturable again (see `MacScreenCapturer::rebuild_stream`).
#[derive(Clone, Debug)]
enum CaptureTarget {
    Display(MonitorInfo),
    Window(u32),
}

/// macOS screen capturer via ScreenCaptureKit.
#[derive(derive_more::Debug)]
pub struct MacScreenCapturer {
    /// Actual pixel dimensions from the CVPixelBuffer callback.
    /// Updated on every frame — authoritative source of truth.
    #[debug(skip)]
    actual_dims: Arc<ActualDimensions>,
    /// The live capture stream and its frame receiver, owned jointly with the
    /// watchdog thread so that thread can replace them during a rebuild.
    ///
    /// `pop_frame` only ever `try_lock`s this: if the watchdog holds it, capture
    /// is mid-rebuild and by definition has no frames to hand over, so returning
    /// `None` immediately is both correct and the only way to guarantee frame
    /// delivery can never block on the rebuild. That matters because rebuilding
    /// calls `SCShareableContent::get()`, which bottoms out in the
    /// `screencapturekit` crate's `SyncCompletion::wait()` — an unbounded
    /// `while !completed { cvar.wait() }` with no timeout and no cancellation.
    /// Doing that work inline on the delivery thread meant a single
    /// ScreenCaptureKit hang would freeze video permanently, which is the exact
    /// failure this recovery machinery exists to prevent.
    #[debug(skip)]
    live: Arc<Mutex<LiveStream>>,
    /// Dimensions the current stream was opened with; fallback for `format()`
    /// before the first frame lands. Shared (not a plain field) because the
    /// watchdog thread updates them when it rebuilds.
    #[debug(skip)]
    requested_dims: Arc<ActualDimensions>,
    /// Signals the watchdog thread to exit when this capturer is dropped.
    #[debug(skip)]
    watchdog_shutdown: Arc<AtomicBool>,
}

/// The mutable half of a capturer: the stream and the channel its frames arrive
/// on. Swapped wholesale by the watchdog on rebuild.
struct LiveStream {
    stream: SCStream,
    rx: mpsc::Receiver<VideoFrame>, // bounded via SyncSender
}

/// How long the capture stream may deliver no frames before it is presumed dead.
///
/// ScreenCaptureKit delivers continuously even on a completely static screen —
/// measured at ~27fps with a maximum inter-frame gap of 226ms over 20s of an
/// untouched desktop (`examples/frame_arrival_pattern.rs`). 3s is >13x that
/// worst observed gap, so this cannot fire on a merely idle screen.
const FRAME_GAP_DEATH: Duration = Duration::from_secs(3);
/// Granularity of the watchdog's sleep, so `Drop` doesn't wait a full interval.
const WATCHDOG_TICK: Duration = Duration::from_millis(250);
/// Minimum spacing between rebuild attempts while the stream stays dead.
const REBUILD_RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// Watches the capture stream's frame heartbeat and raises `needs_rebuild` when
/// frames stop arriving.
///
/// Frame arrival — not target enumeration — is the death signal, for two reasons:
///
/// 1. **It catches strictly more failures.** The live-reported freeze was
///    `SCStream error (System stopped the stream)`, where ScreenCaptureKit tears
///    down the stream but the display *remains perfectly enumerable*. An
///    enumeration-based check is blind to exactly that case, which is the one the
///    user actually hit at the login/lock screen. Frames stopping covers it, and
///    also covers the display genuinely disappearing (frames stop then too).
/// 2. **It is free.** `SCShareableContent::get()` costs 37–54ms
///    (`examples/sck_enumeration_cost.rs`) — longer than a frame at 30fps. Reading
///    an atomic costs nothing, so the healthy path now pays *zero* overhead; the
///    expensive enumeration happens only inside `rebuild_stream`, i.e. only once
///    capture is already known dead and its cost cannot hurt anyone.
///
/// The earlier version of this logic had it backwards on both counts: it polled
/// enumeration every 10s, on the frame-delivery thread, and still missed the
/// actual failure mode.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn spawn_liveness_watchdog(
    epoch: Instant,
    last_frame_ms: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    live: Arc<Mutex<LiveStream>>,
    requested_dims: Arc<ActualDimensions>,
    actual_dims: Arc<ActualDimensions>,
    target: CaptureTarget,
    config: ScreenConfig,
) {
    std::thread::Builder::new()
        .name("sck-liveness-watchdog".into())
        .spawn(move || {
            let mut flagged = false;
            let mut last_attempt: Option<Instant> = None;
            loop {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(WATCHDOG_TICK);
                if shutdown.load(Ordering::Acquire) {
                    return;
                }

                let now_ms = epoch.elapsed().as_millis() as u64;
                let gap =
                    Duration::from_millis(now_ms.saturating_sub(last_frame_ms.load(Ordering::Relaxed)));
                if gap < FRAME_GAP_DEATH {
                    flagged = false;
                    continue;
                }

                if !flagged {
                    flagged = true;
                    warn!(
                        gap_ms = gap.as_millis() as u64,
                        "screen capture delivered no frames for longer than the liveness threshold \
                         (ScreenCaptureKit streams even a static screen, so this means the stream \
                         is dead, not idle) -- rebuilding"
                    );
                }

                let now = Instant::now();
                if let Some(prev) = last_attempt {
                    if now.duration_since(prev) < REBUILD_RETRY_INTERVAL {
                        continue;
                    }
                }
                last_attempt = Some(now);

                match rebuild_stream(&target, &config, epoch, &last_frame_ms, &actual_dims) {
                    Ok((stream, rx, width, height)) => {
                        {
                            let mut slot = live.lock().unwrap_or_else(|e| e.into_inner());
                            // Stop the old stream only once its replacement is
                            // ready, and while holding the slot, so a consumer
                            // never observes a torn state.
                            let _ = slot.stream.stop_capture();
                            slot.stream = stream;
                            slot.rx = rx;
                        }
                        requested_dims.width.store(width, Ordering::Relaxed);
                        requested_dims.height.store(height, Ordering::Relaxed);
                        // Credit the heartbeat so the fresh stream gets a full
                        // window to produce its first frame before being judged.
                        last_frame_ms.store(epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                        info!("screen capture stream rebuilt successfully, resuming");
                    }
                    Err(e) => {
                        // Expected while the Mac is genuinely locked: the target
                        // is not openable yet. Retried on the next interval.
                        tracing::debug!("capture stream rebuild attempt failed, will retry: {e:?}");
                    }
                }
            }
        })
        .ok();
}

impl MacScreenCapturer {
    /// Creates a window capturer for a specific window by ID.
    pub fn new_window(window_id: u32, config: &ScreenConfig) -> Result<Self> {
        check_screen_capture_permission();
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        let window = content
            .windows()
            .into_iter()
            .find(|w| w.window_id() == window_id)
            .context("window not found")?;

        // Window frame is in logical points. On Retina (2x), request double
        // to capture at native pixel resolution. The actual CVPixelBuffer
        // dimensions are tracked via actual_dims from the callback.
        let frame = window.frame();
        let displays = content.displays();
        let cx = frame.x + frame.width / 2.0;
        let cy = frame.y + frame.height / 2.0;
        let scale = displays
            .iter()
            .find(|d| {
                let f = d.frame();
                cx >= f.x && cx < f.x + f.width && cy >= f.y && cy < f.y + f.height
            })
            .map(|d| display_scale_factor(d))
            .unwrap_or(2.0);
        let width = (frame.width * scale) as u32;
        let height = (frame.height * scale) as u32;

        let filter = SCContentFilter::create().with_window(&window).build();

        Self::start_stream(width, height, filter, CaptureTarget::Window(window_id), config)
    }

    /// Creates a screen capturer for the given monitor.
    ///
    /// Captures from the display matching the monitor's ID. Falls back to the
    /// first available display if the ID is not recognized.
    pub fn new(monitor: &MonitorInfo, config: &ScreenConfig) -> Result<Self> {
        check_screen_capture_permission();
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        let display_id: Option<u32> = monitor
            .id
            .strip_prefix("macos-display-")
            .and_then(|s| s.parse().ok());
        let displays = content.displays();
        let display = if let Some(did) = display_id {
            displays
                .into_iter()
                .find(|d| d.display_id() == did)
                .context("display not found")?
        } else {
            displays
                .into_iter()
                .next()
                .context("no displays available")?
        };

        let width = display.width() as u32;
        let height = display.height() as u32;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();

        Self::start_stream(width, height, filter, CaptureTarget::Display(monitor.clone()), config)
    }

    fn start_stream(
        width: u32,
        height: u32,
        filter: SCContentFilter,
        target: CaptureTarget,
        config: &ScreenConfig,
    ) -> Result<Self> {
        let epoch = Instant::now();
        let last_frame_ms = Arc::new(AtomicU64::new(0));
        let actual_dims = Arc::new(ActualDimensions {
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
        });
        let requested_dims = Arc::new(ActualDimensions {
            width: AtomicU32::new(width),
            height: AtomicU32::new(height),
        });
        let (stream, rx) = build_stream(
            width,
            height,
            filter,
            config,
            epoch,
            Arc::clone(&last_frame_ms),
            Arc::clone(&actual_dims),
        )?;

        let live = Arc::new(Mutex::new(LiveStream { stream, rx }));
        let watchdog_shutdown = Arc::new(AtomicBool::new(false));
        spawn_liveness_watchdog(
            epoch,
            Arc::clone(&last_frame_ms),
            Arc::clone(&watchdog_shutdown),
            Arc::clone(&live),
            Arc::clone(&requested_dims),
            Arc::clone(&actual_dims),
            target,
            config.clone(),
        );

        Ok(Self {
            actual_dims,
            live,
            requested_dims,
            watchdog_shutdown,
        })
    }

    /// Test seam: stops the underlying `SCStream` while leaving this wrapper
    /// otherwise untouched, reproducing exactly the live-reported failure
    /// (`SCStream error (System stopped the stream)`) in which ScreenCaptureKit
    /// tears the stream down but the display stays fully enumerable.
    ///
    /// That failure is otherwise only reachable by physically locking the Mac,
    /// which makes the recovery path untestable in an automated way. Exposed so
    /// `examples/recovery_after_stream_death.rs` can prove recovery works.
    #[doc(hidden)]
    pub fn __test_kill_stream(&mut self) {
        let slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let _ = slot.stream.stop_capture();
    }
}

/// Constructs a fresh `SCStream` and the channel its frames arrive on.
///
/// A free function, not a method: the watchdog thread calls it while the
/// capturer it belongs to is being used concurrently by the frame-delivery
/// thread, so it must not require `&self`.
#[cfg(target_os = "macos")]
fn build_stream(
    width: u32,
    height: u32,
    filter: SCContentFilter,
    config: &ScreenConfig,
    epoch: Instant,
    last_frame_ms: Arc<AtomicU64>,
    actual_dims: Arc<ActualDimensions>,
) -> Result<(SCStream, mpsc::Receiver<VideoFrame>)> {
    let mut stream_config = SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_shows_cursor(config.show_cursor)
        .with_pixel_format(PixelFormat::BGRA)
        .with_queue_depth(8);

    if let Some(fps) = config.target_fps {
        stream_config = stream_config.with_minimum_frame_interval(&CMTime::new(1, fps as i32));
    }

    let (frame_tx, frame_rx) = mpsc::sync_channel(2);
    let handler = FrameHandler {
        tx: frame_tx,
        capture_start: epoch,
        actual_dims,
        last_frame_ms,
    };

    let mut stream = SCStream::new(&filter, &stream_config);
    stream.add_output_handler(handler, SCStreamOutputType::Screen);
    stream
        .start_capture()
        .map_err(|e| anyhow::anyhow!("failed to start screen capture: {e:?}"))?;

    info!(width, height, "macOS screen capture started");
    Ok((stream, frame_rx))
}

/// Re-resolves `target` against current ScreenCaptureKit state and builds a
/// replacement stream for it.
///
/// Called ONLY from the watchdog thread. `SCShareableContent::get()` here can
/// block unboundedly — the `screencapturekit` crate waits on a `Condvar` with no
/// timeout — which is precisely why this is not allowed to run on the
/// frame-delivery thread.
#[cfg(target_os = "macos")]
fn rebuild_stream(
    target: &CaptureTarget,
    config: &ScreenConfig,
    epoch: Instant,
    last_frame_ms: &Arc<AtomicU64>,
    actual_dims: &Arc<ActualDimensions>,
) -> Result<(SCStream, mpsc::Receiver<VideoFrame>, u32, u32)> {
    let content = SCShareableContent::get()
        .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

    let (width, height, filter) = match target {
        CaptureTarget::Display(monitor) => {
            let display_id: Option<u32> = monitor
                .id
                .strip_prefix("macos-display-")
                .and_then(|s| s.parse().ok());
            let displays = content.displays();
            let display = if let Some(did) = display_id {
                displays
                    .into_iter()
                    .find(|d| d.display_id() == did)
                    .context("display not found")?
            } else {
                displays.into_iter().next().context("no displays available")?
            };
            let width = display.width() as u32;
            let height = display.height() as u32;
            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build();
            (width, height, filter)
        }
        CaptureTarget::Window(window_id) => {
            let window = content
                .windows()
                .into_iter()
                .find(|w| w.window_id() == *window_id)
                .context("window not found")?;
            let frame = window.frame();
            let displays = content.displays();
            let cx = frame.x + frame.width / 2.0;
            let cy = frame.y + frame.height / 2.0;
            let scale = displays
                .iter()
                .find(|d| {
                    let f = d.frame();
                    cx >= f.x && cx < f.x + f.width && cy >= f.y && cy < f.y + f.height
                })
                .map(|d| display_scale_factor(d))
                .unwrap_or(2.0);
            let width = (frame.width * scale) as u32;
            let height = (frame.height * scale) as u32;
            let filter = SCContentFilter::create().with_window(&window).build();
            (width, height, filter)
        }
    };

    let (stream, rx) = build_stream(
        width,
        height,
        filter,
        config,
        epoch,
        Arc::clone(last_frame_ms),
        Arc::clone(actual_dims),
    )?;
    Ok((stream, rx, width, height))
}

impl Drop for MacScreenCapturer {
    /// Stops the liveness watchdog. Without this the thread outlives every
    /// capturer that ever existed, each one waking forever — a real leak on any
    /// path that constructs capturers repeatedly (display switching, re-broadcast).
    fn drop(&mut self) {
        self.watchdog_shutdown.store(true, Ordering::Release);
    }
}

impl VideoSource for MacScreenCapturer {
    fn name(&self) -> &str {
        "macos-screen"
    }

    fn format(&self) -> VideoFormat {
        // Use actual pixel buffer dimensions if we've received a frame,
        // otherwise fall back to the dimensions the stream was opened with.
        let w = self.actual_dims.width.load(Ordering::Relaxed);
        let h = self.actual_dims.height.load(Ordering::Relaxed);
        let (w, h) = if w > 0 && h > 0 {
            (w, h)
        } else {
            (
                self.requested_dims.width.load(Ordering::Relaxed),
                self.requested_dims.height.load(Ordering::Relaxed),
            )
        };
        VideoFormat {
            pixel_format: RcPixelFormat::Bgra,
            dimensions: [w, h],
        }
    }

    fn start(&mut self) -> Result<()> {
        let slot = self.live.lock().unwrap_or_else(|e| e.into_inner());

        // Drain any frames buffered while no consumer was attached. Without
        // this, the first frame a consumer sees is a STALE boot-era frame
        // whose timestamp then anchors `SharedVideoSource`'s pacing
        // `base_pts` -- making the very next (fresh) frame appear to be
        // "time-since-boot" in the future and scheduling it behind an
        // hours-long `thread::sleep`. Live-witnessed as: exactly one frame
        // (ts=33ms, captured moments after daemon boot) delivered per
        // subscription, then silence forever -- a permanent black screen on
        // every subscriber, with zero errors anywhere.
        while slot.rx.try_recv().is_ok() {}

        // Actually (re)start capture. `new()` starts the stream at
        // construction, and `stop()` (called by `SharedVideoSource` every
        // time the last subscriber detaches) genuinely stops it -- so with
        // the previous no-op `start()`, capture was permanently dead from
        // the first detach onward. SCStream supports restart after
        // stop_capture; an "already running" error is expected and non-fatal.
        if let Err(e) = slot.stream.start_capture() {
            tracing::debug!("start_capture returned {e:?} (already running is expected on first start)");
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Tolerant: a double-stop (or a stop racing SCK's own teardown) is
        // not an actionable error for the shared-source thread.
        let slot = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = slot.stream.stop_capture() {
            tracing::debug!("stop_capture returned {e:?} (already stopped is tolerated)");
        }
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        // `try_lock`, never `lock`: the only other holder is the watchdog
        // thread mid-rebuild, which means capture is dead and there are no
        // frames to return anyway. Blocking here would put an unbounded
        // `SCShareableContent::get()` (a `Condvar` wait with no timeout) on the
        // frame-delivery path, so a single ScreenCaptureKit hang would freeze
        // video permanently -- the exact failure this recovery exists to fix.
        let Ok(slot) = self.live.try_lock() else {
            return Ok(None);
        };
        let mut latest = None;
        while let Ok(frame) = slot.rx.try_recv() {
            latest = Some(frame);
        }
        Ok(latest)
    }
}