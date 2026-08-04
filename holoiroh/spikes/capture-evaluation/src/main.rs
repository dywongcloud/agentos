use rusty_codecs::format::{AppleGpuFrame, GpuFrame, GpuPixelFormat, VideoFrame};
use screencapturekit::cm::CMSampleBufferExt;
use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::SCScreenshotManager;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FPS: u32 = 60;
const QUIESCENCE_MS: u64 = 500;

#[derive(Clone, Copy)]
enum CancelMode {
    Stop,
    Drop,
}

impl CancelMode {
    fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "stop" => Ok(Self::Stop),
            "drop" => Ok(Self::Drop),
            _ => Err(format!("unknown cancellation mode: {value}").into()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Drop => "drop",
        }
    }
}

#[derive(Default)]
struct FrameData {
    callback_ns: Vec<u64>,
    buffer_ns: Vec<u64>,
    status_counts: [u64; 6],
}

struct FrameMetrics {
    origin: Instant,
    cancelling: AtomicBool,
    callbacks_after_cancel: AtomicU64,
    image_buffer_frames: AtomicU64,
    iosurface_frames: AtomicU64,
    adapter_native_handle_frames: AtomicU64,
    data: Mutex<FrameData>,
}

impl FrameMetrics {
    fn new(origin: Instant) -> Self {
        Self {
            origin,
            cancelling: AtomicBool::new(false),
            callbacks_after_cancel: AtomicU64::new(0),
            image_buffer_frames: AtomicU64::new(0),
            iosurface_frames: AtomicU64::new(0),
            adapter_native_handle_frames: AtomicU64::new(0),
            data: Mutex::new(FrameData::default()),
        }
    }
}

struct Handler {
    metrics: Arc<FrameMetrics>,
}

impl SCStreamOutputTrait for Handler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output_type: SCStreamOutputType) {
        if !matches!(output_type, SCStreamOutputType::Screen) {
            return;
        }
        if self.metrics.cancelling.load(Ordering::Acquire) {
            self.metrics
                .callbacks_after_cancel
                .fetch_add(1, Ordering::Relaxed);
        }
        let elapsed = self.metrics.origin.elapsed();
        if let Some(buffer) = sample.image_buffer() {
            self.metrics
                .data
                .lock()
                .unwrap()
                .buffer_ns
                .push(elapsed.as_nanos() as u64);
            self.metrics
                .image_buffer_frames
                .fetch_add(1, Ordering::Relaxed);
            if buffer.is_backed_by_io_surface() {
                self.metrics
                    .iosurface_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
            let width = buffer.width() as u32;
            let height = buffer.height() as u32;
            let apple = unsafe {
                AppleGpuFrame::from_raw(buffer.as_ptr(), width, height, GpuPixelFormat::Bgra)
            };
            let frame = VideoFrame::new_gpu(GpuFrame::new(Arc::new(apple)), elapsed);
            if frame.native_handle().is_some() {
                self.metrics
                    .adapter_native_handle_frames
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        let status = sample.frame_status().unwrap_or_default() as usize;
        let mut data = self.metrics.data.lock().unwrap();
        data.callback_ns.push(elapsed.as_nanos() as u64);
        if status < data.status_counts.len() {
            data.status_counts[status] += 1;
        }
    }
}

#[derive(Clone, Copy)]
struct Usage {
    user_s: f64,
    system_s: f64,
    max_rss_bytes: i64,
}

fn usage() -> Usage {
    let mut value = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) };
    if result != 0 {
        return Usage {
            user_s: 0.0,
            system_s: 0.0,
            max_rss_bytes: 0,
        };
    }
    let value = unsafe { value.assume_init() };
    Usage {
        user_s: value.ru_utime.tv_sec as f64 + value.ru_utime.tv_usec as f64 / 1_000_000.0,
        system_s: value.ru_stime.tv_sec as f64 + value.ru_stime.tv_usec as f64 / 1_000_000.0,
        max_rss_bytes: value.ru_maxrss,
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[rank]
}

fn enumerate() -> Result<(SCShareableContent, usize, usize, Duration), Box<dyn Error>> {
    let started = Instant::now();
    let content = SCShareableContent::get()?;
    let elapsed = started.elapsed();
    let display_count = content.displays().len();
    let window_count = content.windows().len();
    Ok((content, display_count, window_count, elapsed))
}

fn list_content() -> Result<(), Box<dyn Error>> {
    let (content, display_count, window_count, elapsed) = enumerate()?;
    let snapshot_started = Instant::now();
    let snapshot = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| content.snapshot()));
    let snapshot_elapsed = snapshot_started.elapsed();
    println!("operation=enumerate");
    println!("enumeration_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
    println!("display_count={display_count}");
    println!("window_count={window_count}");
    println!("snapshot_ms={:.3}", snapshot_elapsed.as_secs_f64() * 1000.0);
    match snapshot {
        Ok(Some(snapshot)) => {
            println!("snapshot_result=ok");
            println!("snapshot_display_count={}", snapshot.displays.len());
            println!("snapshot_window_count={}", snapshot.windows.len());
            println!("snapshot_application_count={}", snapshot.applications.len());
            for (index, display) in snapshot.displays.iter().enumerate() {
                println!(
                    "display_{index}=id:{},width:{},height:{}",
                    display.display_id, display.width, display.height
                );
            }
        }
        Ok(None) => println!("snapshot_result=none"),
        Err(_) => println!("snapshot_result=panic:index-out-of-bounds"),
    }
    Ok(())
}

fn select_display(content: &SCShareableContent, index: usize) -> Result<SCDisplay, Box<dyn Error>> {
    content
        .displays()
        .into_iter()
        .nth(index)
        .ok_or_else(|| format!("display index {index} is not available").into())
}

fn filter_for(display: &SCDisplay) -> SCContentFilter {
    SCContentFilter::create()
        .with_display(display)
        .with_excluding_windows(&[])
        .build()
}

fn config(width: u32, height: u32) -> SCStreamConfiguration {
    SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_pixel_format(PixelFormat::BGRA)
        .with_fps(FPS)
        .with_queue_depth(8)
        .with_shows_cursor(true)
}

fn snapshot(display_index: usize) -> Result<(), Box<dyn Error>> {
    let total_started = Instant::now();
    let (content, display_count, window_count, enumeration) = enumerate()?;
    let display = select_display(&content, display_index)?;
    let width = display.width();
    let height = display.height();
    let filter = filter_for(&display);
    let config = config(width, height);
    let capture_started = Instant::now();
    let image = SCScreenshotManager::capture_image(&filter, &config)?;
    let capture_elapsed = capture_started.elapsed();
    println!("operation=snapshot");
    println!("display_index={display_index}");
    println!("display_count={display_count}");
    println!("window_count={window_count}");
    println!("configured_width={width}");
    println!("configured_height={height}");
    println!("enumeration_ms={:.3}", enumeration.as_secs_f64() * 1000.0);
    println!("snapshot_ms={:.3}", capture_elapsed.as_secs_f64() * 1000.0);
    println!("image_width={}", image.width());
    println!("image_height={}", image.height());
    println!(
        "total_ms={:.3}",
        total_started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

fn stream(display_index: usize, duration_s: u64, mode: CancelMode) -> Result<(), Box<dyn Error>> {
    let wall_started = Instant::now();
    let usage_started = usage();
    let (content, display_count, window_count, enumeration) = enumerate()?;
    let display = select_display(&content, display_index)?;
    let width = display.width();
    let height = display.height();
    let filter = filter_for(&display);
    let configuration = config(width, height);
    let metrics = Arc::new(FrameMetrics::new(Instant::now()));
    let handler = Handler {
        metrics: metrics.clone(),
    };
    let mut capture = SCStream::new(&filter, &configuration);
    capture.add_output_handler(handler, SCStreamOutputType::Screen);
    let start_called = Instant::now();
    capture.start_capture()?;
    let start_call_elapsed = start_called.elapsed();
    std::thread::sleep(Duration::from_secs(duration_s));
    metrics.cancelling.store(true, Ordering::Release);
    let cancel_started = Instant::now();
    match mode {
        CancelMode::Stop => capture.stop_capture()?,
        CancelMode::Drop => drop(capture),
    }
    let cancel_elapsed = cancel_started.elapsed();
    std::thread::sleep(Duration::from_millis(QUIESCENCE_MS));
    let usage_finished = usage();
    let wall_elapsed = wall_started.elapsed();
    let data = metrics.data.lock().unwrap();
    let callback_ns = &data.callback_ns;
    let buffer_ns = &data.buffer_ns;
    let mut callback_interval_ms: Vec<f64> = callback_ns
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f64 / 1_000_000.0)
        .collect();
    callback_interval_ms.sort_by(f64::total_cmp);
    let mut delivery_interval_ms: Vec<f64> = buffer_ns
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f64 / 1_000_000.0)
        .collect();
    delivery_interval_ms.sort_by(f64::total_cmp);
    let first_frame_ms = buffer_ns
        .first()
        .map(|value| *value as f64 / 1_000_000.0)
        .unwrap_or(0.0);
    let observed_span_s = callback_ns
        .first()
        .zip(callback_ns.last())
        .map(|(first, last)| (last - first) as f64 / 1_000_000_000.0)
        .unwrap_or(0.0);
    let expected_callbacks = if callback_ns.is_empty() {
        0
    } else {
        (observed_span_s * FPS as f64).round() as u64 + 1
    };
    let callback_count = callback_ns.len() as u64;
    let estimated_missing_callbacks = expected_callbacks.saturating_sub(callback_count);
    let user_s = usage_finished.user_s - usage_started.user_s;
    let system_s = usage_finished.system_s - usage_started.system_s;
    let cpu_percent = if wall_elapsed.is_zero() {
        0.0
    } else {
        (user_s + system_s) / wall_elapsed.as_secs_f64() * 100.0
    };
    println!("operation=stream");
    println!("cancel_mode={}", mode.name());
    println!("display_index={display_index}");
    println!("display_count={display_count}");
    println!("window_count={window_count}");
    println!("configured_width={width}");
    println!("configured_height={height}");
    println!("configured_fps={FPS}");
    println!("queue_depth=8");
    println!("duration_s={duration_s}");
    println!("quiescence_ms={QUIESCENCE_MS}");
    println!("enumeration_ms={:.3}", enumeration.as_secs_f64() * 1000.0);
    println!(
        "start_call_ms={:.3}",
        start_call_elapsed.as_secs_f64() * 1000.0
    );
    println!("first_frame_ms={first_frame_ms:.3}");
    println!("callbacks={callback_count}");
    println!("observed_span_s={observed_span_s:.6}");
    println!("expected_callbacks={expected_callbacks}");
    println!("estimated_missing_callbacks={estimated_missing_callbacks}");
    println!(
        "callback_interval_p50_ms={:.3}",
        percentile(&callback_interval_ms, 0.50)
    );
    println!(
        "callback_interval_p95_ms={:.3}",
        percentile(&callback_interval_ms, 0.95)
    );
    println!(
        "callback_interval_p99_ms={:.3}",
        percentile(&callback_interval_ms, 0.99)
    );
    println!(
        "delivery_interval_p50_ms={:.3}",
        percentile(&delivery_interval_ms, 0.50)
    );
    println!(
        "delivery_interval_p95_ms={:.3}",
        percentile(&delivery_interval_ms, 0.95)
    );
    println!(
        "delivery_interval_p99_ms={:.3}",
        percentile(&delivery_interval_ms, 0.99)
    );
    println!("status_complete={}", data.status_counts[0]);
    println!("status_idle={}", data.status_counts[1]);
    println!("status_blank={}", data.status_counts[2]);
    println!("status_suspended={}", data.status_counts[3]);
    println!("status_started={}", data.status_counts[4]);
    println!("status_stopped={}", data.status_counts[5]);
    println!(
        "image_buffer_frames={}",
        metrics.image_buffer_frames.load(Ordering::Relaxed)
    );
    println!(
        "iosurface_frames={}",
        metrics.iosurface_frames.load(Ordering::Relaxed)
    );
    println!(
        "adapter_native_handle_frames={}",
        metrics.adapter_native_handle_frames.load(Ordering::Relaxed)
    );
    println!(
        "callbacks_after_cancel={}",
        metrics.callbacks_after_cancel.load(Ordering::Relaxed)
    );
    println!(
        "cancel_call_ms={:.3}",
        cancel_elapsed.as_secs_f64() * 1000.0
    );
    println!("user_cpu_s={user_s:.6}");
    println!("system_cpu_s={system_s:.6}");
    println!("cpu_percent={cpu_percent:.3}");
    println!("max_rss_bytes={}", usage_finished.max_rss_bytes);
    println!("wall_s={:.6}", wall_elapsed.as_secs_f64());
    Ok(())
}

fn permission_probe(display_index: usize) -> Result<(), Box<dyn Error>> {
    println!("operation=permission_probe");
    match enumerate() {
        Ok((_, displays, windows, elapsed)) => {
            println!("permission_context=allowed");
            println!("display_count={displays}");
            println!("window_count={windows}");
            println!("enumeration_ms={:.3}", elapsed.as_secs_f64() * 1000.0);
            stream(display_index, 1, CancelMode::Stop)?;
            stream(display_index, 1, CancelMode::Drop)?;
        }
        Err(error) => {
            println!("permission_context=denied");
            println!("enumeration_error={error:?}");
            println!("stream_created=false");
            println!("cancellation_reachable=false");
        }
    }
    Ok(())
}

fn parse_usize(value: Option<String>, default: usize) -> Result<usize, Box<dyn Error>> {
    Ok(value.map_or(Ok(default), |item| item.parse())?)
}

fn parse_u64(value: Option<String>, default: u64) -> Result<u64, Box<dyn Error>> {
    Ok(value.map_or(Ok(default), |item| item.parse())?)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("list") => list_content(),
        Some("snapshot") => snapshot(parse_usize(args.next(), 0)?),
        Some("stream") => {
            let display_index = parse_usize(args.next(), 0)?;
            let duration_s = parse_u64(args.next(), 10)?;
            let mode = CancelMode::parse(args.next().as_deref().unwrap_or("stop"))?;
            stream(display_index, duration_s, mode)
        }
        Some("permission-probe") => permission_probe(parse_usize(args.next(), 0)?),
        _ => Err("usage: capture-evaluation-spike list | snapshot [display] | stream [display] [seconds] [stop|drop] | permission-probe [display]".into()),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("result=error");
        eprintln!("error={error:?}");
        std::process::exit(1);
    }
}
