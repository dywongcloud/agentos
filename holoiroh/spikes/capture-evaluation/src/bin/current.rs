use rusty_capture::{ScreenCapturer, ScreenConfig, VideoSource};
use std::error::Error;
use std::time::{Duration, Instant};

const FPS: f32 = 60.0;
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

fn list() -> Result<(), Box<dyn Error>> {
    let display_started = Instant::now();
    let displays = ScreenCapturer::list_all()?;
    let display_elapsed = display_started.elapsed();
    let window_started = Instant::now();
    let windows = ScreenCapturer::list_windows()?;
    let window_elapsed = window_started.elapsed();
    println!("implementation=current-rusty-capture");
    println!("operation=enumerate");
    println!(
        "display_enumeration_ms={:.3}",
        display_elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "window_enumeration_ms={:.3}",
        window_elapsed.as_secs_f64() * 1000.0
    );
    println!("display_count={}", displays.len());
    println!("window_count={}", windows.len());
    for (index, display) in displays.iter().enumerate() {
        println!(
            "display_{index}=id:{},width:{},height:{},scale:{}",
            display.id, display.dimensions[0], display.dimensions[1], display.scale_factor
        );
    }
    Ok(())
}

fn stream(display_index: usize, duration_s: u64, mode: CancelMode) -> Result<(), Box<dyn Error>> {
    let wall_started = Instant::now();
    let usage_started = usage();
    let enumeration_started = Instant::now();
    let displays = ScreenCapturer::list_all()?;
    let enumeration_elapsed = enumeration_started.elapsed();
    let display = displays
        .get(display_index)
        .ok_or_else(|| format!("display index {display_index} is not available"))?;
    let config = ScreenConfig {
        target_fps: Some(FPS),
        ..ScreenConfig::default()
    };
    let construct_started = Instant::now();
    let mut capture = ScreenCapturer::with_monitor_config(display, &config)?;
    let construct_elapsed = construct_started.elapsed();
    let start_started = Instant::now();
    capture.start()?;
    let start_elapsed = start_started.elapsed();
    let delivery_started = Instant::now();
    let deadline = delivery_started + Duration::from_secs(duration_s);
    let mut frame_ns = Vec::new();
    let mut gpu_frames = 0u64;
    let mut native_handle_frames = 0u64;
    let mut width = 0u32;
    let mut height = 0u32;
    while Instant::now() < deadline {
        if let Some(frame) = capture.pop_frame()? {
            let elapsed = delivery_started.elapsed().as_nanos() as u64;
            frame_ns.push(elapsed);
            width = frame.width();
            height = frame.height();
            if frame.is_gpu() {
                gpu_frames += 1;
            }
            if frame.native_handle().is_some() {
                native_handle_frames += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let cancel_started = Instant::now();
    let cancel_call_elapsed;
    let mut post_cancel_frames = 0u64;
    match mode {
        CancelMode::Stop => {
            capture.stop()?;
            cancel_call_elapsed = cancel_started.elapsed();
            let quiescence_deadline = Instant::now() + Duration::from_millis(QUIESCENCE_MS);
            while Instant::now() < quiescence_deadline {
                if capture.pop_frame()?.is_some() {
                    post_cancel_frames += 1;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(capture);
        }
        CancelMode::Drop => {
            drop(capture);
            cancel_call_elapsed = cancel_started.elapsed();
            std::thread::sleep(Duration::from_millis(QUIESCENCE_MS));
        }
    }
    let cancel_elapsed = cancel_started.elapsed();
    let usage_finished = usage();
    let wall_elapsed = wall_started.elapsed();
    let mut delivery_interval_ms: Vec<f64> = frame_ns
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f64 / 1_000_000.0)
        .collect();
    delivery_interval_ms.sort_by(f64::total_cmp);
    let first_frame_ms = frame_ns
        .first()
        .map(|value| *value as f64 / 1_000_000.0)
        .unwrap_or(0.0);
    let observed_span_s = frame_ns
        .first()
        .zip(frame_ns.last())
        .map(|(first, last)| (last - first) as f64 / 1_000_000_000.0)
        .unwrap_or(0.0);
    let expected_frames = if frame_ns.is_empty() {
        0
    } else {
        (observed_span_s * FPS as f64).round() as u64 + 1
    };
    let frames = frame_ns.len() as u64;
    let estimated_missing_frames = expected_frames.saturating_sub(frames);
    let user_s = usage_finished.user_s - usage_started.user_s;
    let system_s = usage_finished.system_s - usage_started.system_s;
    let cpu_percent = if wall_elapsed.is_zero() {
        0.0
    } else {
        (user_s + system_s) / wall_elapsed.as_secs_f64() * 100.0
    };
    println!("implementation=current-rusty-capture");
    println!("operation=stream");
    println!("cancel_mode={}", mode.name());
    println!("display_index={display_index}");
    println!("display_count={}", displays.len());
    println!("configured_width={}", display.dimensions[0]);
    println!("configured_height={}", display.dimensions[1]);
    println!("configured_fps={FPS}");
    println!("duration_s={duration_s}");
    println!("quiescence_ms={QUIESCENCE_MS}");
    println!(
        "enumeration_ms={:.3}",
        enumeration_elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "construct_ms={:.3}",
        construct_elapsed.as_secs_f64() * 1000.0
    );
    println!("start_call_ms={:.3}", start_elapsed.as_secs_f64() * 1000.0);
    println!("first_frame_after_start_ms={first_frame_ms:.3}");
    println!("actual_width={width}");
    println!("actual_height={height}");
    println!("frames={frames}");
    println!("observed_span_s={observed_span_s:.6}");
    println!("expected_frames={expected_frames}");
    println!("estimated_missing_frames={estimated_missing_frames}");
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
    println!("gpu_frames={gpu_frames}");
    println!("native_handle_frames={native_handle_frames}");
    println!("post_cancel_frames={post_cancel_frames}");
    println!(
        "cancel_call_ms={:.3}",
        cancel_call_elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "cancel_and_quiescence_ms={:.3}",
        cancel_elapsed.as_secs_f64() * 1000.0
    );
    println!("user_cpu_s={user_s:.6}");
    println!("system_cpu_s={system_s:.6}");
    println!("cpu_percent={cpu_percent:.3}");
    println!("max_rss_bytes={}", usage_finished.max_rss_bytes);
    println!("wall_s={:.6}", wall_elapsed.as_secs_f64());
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
        Some("list") => list(),
        Some("stream") => {
            let display_index = parse_usize(args.next(), 0)?;
            let duration_s = parse_u64(args.next(), 10)?;
            let mode = CancelMode::parse(args.next().as_deref().unwrap_or("stop"))?;
            stream(display_index, duration_s, mode)
        }
        _ => Err("usage: current list | stream [display] [seconds] [stop|drop]".into()),
    }
}

fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
    if let Err(error) = run() {
        eprintln!("result=error");
        eprintln!("error={error:?}");
        std::process::exit(1);
    }
}
