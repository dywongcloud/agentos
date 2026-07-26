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
        mpsc,
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
    /// Requested dimensions (logical points for windows, display pixels for screens).
    /// Used as fallback before the first frame arrives.
    requested_width: u32,
    requested_height: u32,
    /// Actual pixel dimensions from the CVPixelBuffer callback.
    /// Updated on every frame — authoritative source of truth.
    #[debug(skip)]
    actual_dims: Arc<ActualDimensions>,
    #[debug(skip)]
    rx: mpsc::Receiver<VideoFrame>, // bounded via SyncSender
    #[debug(skip)]
    stream: SCStream,
    target: CaptureTarget,
    #[debug(skip)]
    config: ScreenConfig,
    /// Raised by `spawn_liveness_watchdog` when frames stop arriving.
    /// `pop_frame` only ever does a cheap atomic load of this.
    #[debug(skip)]
    needs_rebuild: Arc<AtomicBool>,
    /// Signals the watchdog thread to exit when this capturer is dropped.
    #[debug(skip)]
    watchdog_shutdown: Arc<AtomicBool>,
    /// Shared timebase for frame timestamps AND the liveness heartbeat.
    /// Deliberately preserved across `rebuild_stream` so frame timestamps do not
    /// jump backwards to ~0 after a recovery — a backwards jump would re-anchor
    /// the downstream pacing clock and stall playout.
    #[debug(skip)]
    epoch: Instant,
    /// Millis-since-`epoch` of the last delivered frame; written by `FrameHandler`,
    /// read by the watchdog.
    #[debug(skip)]
    last_frame_ms: Arc<AtomicU64>,
    /// Rate-limits *rebuild attempts* (not detection) so a target that stays
    /// unopenable can't spin `pop_frame` into a tight rebuild loop.
    last_rebuild_attempt: Option<Instant>,
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
fn spawn_liveness_watchdog(
    epoch: Instant,
    last_frame_ms: Arc<AtomicU64>,
    needs_rebuild: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("sck-liveness-watchdog".into())
        .spawn(move || {
            loop {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(WATCHDOG_TICK);
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                let now_ms = epoch.elapsed().as_millis() as u64;
                let last_ms = last_frame_ms.load(Ordering::Relaxed);
                let gap = Duration::from_millis(now_ms.saturating_sub(last_ms));
                if gap < FRAME_GAP_DEATH {
                    continue;
                }
                if !needs_rebuild.swap(true, Ordering::AcqRel) {
                    warn!(
                        gap_ms = gap.as_millis() as u64,
                        "screen capture delivered no frames for longer than the liveness threshold \
                         (ScreenCaptureKit streams even a static screen, so this means the stream \
                         is dead, not idle) -- flagging for rebuild"
                    );
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
        let (stream, rx, actual_dims) =
            Self::build_stream(width, height, filter, config, epoch, Arc::clone(&last_frame_ms))?;

        let needs_rebuild = Arc::new(AtomicBool::new(false));
        let watchdog_shutdown = Arc::new(AtomicBool::new(false));
        spawn_liveness_watchdog(
            epoch,
            Arc::clone(&last_frame_ms),
            Arc::clone(&needs_rebuild),
            Arc::clone(&watchdog_shutdown),
        );

        Ok(Self {
            requested_width: width,
            requested_height: height,
            actual_dims,
            rx,
            stream,
            target,
            config: config.clone(),
            needs_rebuild,
            watchdog_shutdown,
            epoch,
            last_frame_ms,
            last_rebuild_attempt: None,
        })
    }

    /// Constructs a fresh `SCStream` (+ its frame channel/dims) for the given
    /// resolved filter. Pure construction, no `Self` bookkeeping — shared by
    /// `start_stream` (initial construction) and `rebuild_stream` (recovery).
    fn build_stream(
        width: u32,
        height: u32,
        filter: SCContentFilter,
        config: &ScreenConfig,
        epoch: Instant,
        last_frame_ms: Arc<AtomicU64>,
    ) -> Result<(SCStream, mpsc::Receiver<VideoFrame>, Arc<ActualDimensions>)> {
        let mut stream_config = SCStreamConfiguration::new()
            .with_width(width)
            .with_height(height)
            .with_shows_cursor(config.show_cursor)
            .with_pixel_format(PixelFormat::BGRA)
            .with_queue_depth(8);

        if let Some(fps) = config.target_fps {
            stream_config = stream_config.with_minimum_frame_interval(&CMTime::new(1, fps as i32));
        }

        let actual_dims = Arc::new(ActualDimensions {
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
        });

        let (frame_tx, frame_rx) = mpsc::sync_channel(2);
        let handler = FrameHandler {
            tx: frame_tx,
            capture_start: epoch,
            actual_dims: Arc::clone(&actual_dims),
            last_frame_ms,
        };

        let mut stream = SCStream::new(&filter, &stream_config);
        stream.add_output_handler(handler, SCStreamOutputType::Screen);
        stream
            .start_capture()
            .map_err(|e| anyhow::anyhow!("failed to start screen capture: {e:?}"))?;

        info!(width, height, "macOS screen capture started");

        Ok((stream, frame_rx, actual_dims))
    }

    /// Live-witnessed root cause of a permanently frozen live-share view:
    /// when the Mac reaches its login/lock screen, ScreenCaptureKit can stop
    /// the running `SCStream` out from under this wrapper (`SCStream error
    /// (System stopped the stream)`) or fail to find any capturable
    /// display/window at all (`SCStream error (No capture source
    /// provided)`) — both observed as raw diagnostic prints from the
    /// `screencapturekit` crate/framework, NOT as a `Result` this Rust code
    /// can catch. Frames simply stop arriving forever: the last frame stays
    /// frozen on every viewer's screen, with zero errors surfaced anywhere,
    /// since neither this struct nor `SharedVideoSource`'s poll loop
    /// (`moq-media/src/publish.rs`) previously noticed.
    ///
    /// Detection is deliberately NOT frame-arrival timing: ScreenCaptureKit is
    /// dirty-region-driven, not a fixed-rate camera, so genuinely static screen
    /// content legitimately produces no frames for long stretches — treating
    /// that as failure would rebuild a perfectly healthy stream. It is instead
    /// the presence of this capturer's own target in `SCShareableContent`, the
    /// same unambiguous signal ScreenCaptureKit's uncatchable error reports.
    ///
    /// That enumeration costs 37–54ms (measured: `examples/sck_enumeration_cost.rs`),
    /// which is longer than a 30fps frame, so it runs on `spawn_target_watchdog`'s
    /// own thread and this method only does a cheap atomic load of the result.
    /// Rebuilding itself is done here because it needs `&mut self`; its own
    /// enumeration cost is irrelevant since by definition capture is already dead.
    ///
    /// Never returns `Err`: `SharedVideoSource`'s poll loop treats any `Err`
    /// from `pop_frame` as fatal and permanently kills the capture thread, so a
    /// failed rebuild must only be logged and retried.
    fn try_recover_if_stream_dead(&mut self) {
        if !self.needs_rebuild.load(Ordering::Acquire) {
            return;
        }

        let now = Instant::now();
        if let Some(last) = self.last_rebuild_attempt {
            if now.duration_since(last) < REBUILD_RETRY_INTERVAL {
                return;
            }
        }
        self.last_rebuild_attempt = Some(now);

        match self.rebuild_stream() {
            Ok(()) => {
                info!("screen capture stream rebuilt successfully, resuming");
                // Credit the heartbeat now so the watchdog gives the fresh stream
                // a full FRAME_GAP_DEATH window to produce its first frame instead
                // of immediately re-flagging it as dead.
                self.last_frame_ms
                    .store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                self.needs_rebuild.store(false, Ordering::Release);
            }
            Err(e) => {
                // Expected while the Mac is genuinely locked: the target is not
                // openable yet. Retried every REBUILD_RETRY_INTERVAL until it is.
                tracing::debug!("capture stream rebuild attempt failed, will retry: {e:?}");
            }
        }
    }

    /// Tears down the current (dead) `SCStream` and constructs a fresh one
    /// against the same retained target/config — the instance-method
    /// equivalent of `new`/`new_window`, reused by `try_recover_if_stream_dead`.
    fn rebuild_stream(&mut self) -> Result<()> {
        let content = SCShareableContent::get()
            .map_err(|e| anyhow::anyhow!("ScreenCaptureKit: failed to get content: {e:?}"))?;

        let (width, height, filter) = match &self.target {
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

        // Stop the old stream BEFORE starting the replacement. Building first
        // would briefly run two SCStreams against the same display, doubling
        // capture cost and letting the doomed stream race frames into the new
        // handler. Best-effort: the old stream is almost certainly already dead,
        // so a stop() failure here is expected and not actionable.
        if let Err(e) = self.stream.stop_capture() {
            tracing::debug!("stop_capture on the dead stream returned {e:?} (expected)");
        }

        let (stream, rx, actual_dims) = Self::build_stream(
            width,
            height,
            filter,
            &self.config,
            self.epoch,
            Arc::clone(&self.last_frame_ms),
        )?;

        self.stream = stream;
        self.rx = rx;
        self.actual_dims = actual_dims;
        self.requested_width = width;
        self.requested_height = height;
        Ok(())
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
        let _ = self.stream.stop_capture();
    }
}

impl Drop for MacScreenCapturer {
    /// Stops the target watchdog. Without this the thread outlives every
    /// capturer that ever existed, each one waking to make a ~40ms
    /// `SCShareableContent` call forever — a real leak on any code path that
    /// constructs capturers repeatedly (display switching, re-broadcast).
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
        // otherwise fall back to the requested dimensions.
        let w = self.actual_dims.width.load(Ordering::Relaxed);
        let h = self.actual_dims.height.load(Ordering::Relaxed);
        let (w, h) = if w > 0 && h > 0 {
            (w, h)
        } else {
            (self.requested_width, self.requested_height)
        };
        VideoFormat {
            pixel_format: RcPixelFormat::Bgra,
            dimensions: [w, h],
        }
    }

    fn start(&mut self) -> Result<()> {
        // Drain any frames buffered while no consumer was attached. Without
        // this, the first frame a consumer sees is a STALE boot-era frame
        // whose timestamp then anchors `SharedVideoSource`'s pacing
        // `base_pts` -- making the very next (fresh) frame appear to be
        // "time-since-boot" in the future and scheduling it behind an
        // hours-long `thread::sleep`. Live-witnessed as: exactly one frame
        // (ts=33ms, captured moments after daemon boot) delivered per
        // subscription, then silence forever -- a permanent black screen on
        // every subscriber, with zero errors anywhere.
        while self.rx.try_recv().is_ok() {}

        // Actually (re)start capture. `new()` starts the stream at
        // construction, and `stop()` (called by `SharedVideoSource` every
        // time the last subscriber detaches) genuinely stops it -- so with
        // the previous no-op `start()`, capture was permanently dead from
        // the first detach onward (matching the live-witnessed "Failed to
        // stop a stream that is already stopped or does not exist" warnings
        // on every later detach). SCStream supports restart after
        // stop_capture; an "already running" error (first-ever start, where
        // `new()` already started it) is expected and non-fatal.
        if let Err(e) = self.stream.start_capture() {
            tracing::debug!("start_capture returned {e:?} (already running is expected on first start)");
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Tolerant: a double-stop (or a stop racing SCK's own teardown) is
        // not an actionable error for the shared-source thread -- it just
        // warns and parks either way. Surface it at debug, not as an Err.
        if let Err(e) = self.stream.stop_capture() {
            tracing::debug!("stop_capture returned {e:?} (already stopped is tolerated)");
        }
        Ok(())
    }

    fn pop_frame(&mut self) -> Result<Option<VideoFrame>> {
        let mut latest = None;
        while let Ok(frame) = self.rx.try_recv() {
            latest = Some(frame);
        }
        if latest.is_none() {
            self.try_recover_if_stream_dead();
        }
        Ok(latest)
    }
}
