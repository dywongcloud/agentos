//! Measures how long a pixel takes to get from the Mac to the phone's decoder.
//!
//! The input path is now microseconds end to end (`input_latency_probe`), so what a user calls
//! "the cursor is laggy" is almost entirely video feedback: the cursor appears to move late
//! because the PICTURE of it arrives late. That claim was reached by inspection, and inspection
//! also produced a ranked list of suspects. This makes them measurable.
//!
//! Both ends run in one process over a real loopback iroh connection, so publisher and subscriber
//! timestamps share a clock. The source is not the built-in `TestPatternSource`, whose PTS is
//! virtual (frame_index / fps) and which yields a frame on every poll: latency against a virtual
//! clock is not latency. `PacedProbeSource` instead emits on a REAL schedule and stamps each frame
//! with real elapsed time, so `now - frame.timestamp` at the far end is the true pipeline delay
//! through encode, transport, and decode.
//!
//! It also moves a band across the frame every frame, because a static picture encodes to almost
//! nothing and would flatter every measurement below.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::bytes::Bytes;
use iroh_live::media::codec::VideoCodec;
use iroh_live::media::format::{PixelFormat, VideoFormat, VideoFrame, VideoPreset};
use iroh_live::media::publish::LocalBroadcast;
use iroh_live::media::traits::VideoSource;
use iroh_live::Live;

const BROADCAST_NAME: &str = "video-latency-probe";
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const MEASURE_FOR: Duration = Duration::from_secs(6);
const SETTLE: Duration = Duration::from_secs(2);

/// Emits frames on a real schedule, stamped with real elapsed time.
struct PacedProbeSource {
    format: VideoFormat,
    fps: f64,
    started: Option<Instant>,
    emitted: u64,
    buffer: Vec<u8>,
}

impl PacedProbeSource {
    fn new(fps: f64) -> Self {
        Self {
            format: VideoFormat {
                pixel_format: PixelFormat::Rgba,
                dimensions: [WIDTH, HEIGHT],
            },
            fps,
            started: None,
            emitted: 0,
            buffer: vec![0u8; (WIDTH * HEIGHT * 4) as usize],
        }
    }

    /// A moving band plus per-frame noise in one row, so every frame carries genuinely new
    /// information for the encoder rather than compressing away to nothing.
    fn paint(&mut self, frame_index: u64) {
        self.buffer.fill(24);
        let band = ((frame_index * 13) % u64::from(WIDTH - 64)) as u32;
        for y in 0..HEIGHT {
            for x in band..(band + 64).min(WIDTH) {
                let idx = ((y * WIDTH + x) * 4) as usize;
                self.buffer[idx] = 240;
                self.buffer[idx + 1] = 200;
                self.buffer[idx + 2] = 40;
                self.buffer[idx + 3] = 255;
            }
        }
        for x in 0..WIDTH {
            let idx = ((x % HEIGHT) * WIDTH * 4 + x * 4) as usize;
            self.buffer[idx] = (frame_index.wrapping_mul(7) % 255) as u8;
        }
    }
}

impl VideoSource for PacedProbeSource {
    fn name(&self) -> &str {
        "paced-probe"
    }

    fn format(&self) -> VideoFormat {
        self.format.clone()
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.started = Some(Instant::now());
        self.emitted = 0;
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.started = None;
        Ok(())
    }

    fn pop_frame(&mut self) -> anyhow::Result<Option<VideoFrame>> {
        let Some(started) = self.started else {
            return Ok(None);
        };
        let elapsed = started.elapsed();
        let due = Duration::from_secs_f64(self.emitted as f64 / self.fps);
        if elapsed < due {
            return Ok(None);
        }
        let index = self.emitted;
        self.emitted += 1;
        self.paint(index);
        Ok(Some(VideoFrame::new_rgba(
            Bytes::copy_from_slice(&self.buffer),
            WIDTH,
            HEIGHT,
            elapsed,
        )))
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

struct Run {
    latencies: Vec<Duration>,
    delivered: usize,
    source_fps: f64,
}

async fn measure(source_fps: f64) -> anyhow::Result<Run> {
    let publisher = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await?;
    let subscriber = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await?;

    let publisher_addr = iroh::EndpointAddr::from_parts(
        publisher.id(),
        publisher.bound_sockets().into_iter().map(|mut s| {
            if s.ip().is_unspecified() {
                s.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
            }
            iroh::TransportAddr::Ip(s)
        }),
    );

    let publisher_live = Live::builder(publisher).with_router().spawn();
    let subscriber_live = Live::builder(subscriber).with_router().spawn();

    let broadcast = LocalBroadcast::new();
    let codec = VideoCodec::best_available().unwrap_or(VideoCodec::H264);
    let started_at = Arc::new(Mutex::new(None::<Instant>));
    {
        let started_at = started_at.clone();
        let mut source = PacedProbeSource::new(source_fps);
        source.start()?;
        *started_at.lock().unwrap() = source.started;
        broadcast
            .video()
            .set_source(source, codec, vec![VideoPreset::P720])?;
    }
    publisher_live.publish(BROADCAST_NAME, &broadcast).await?;

    let subscription = subscriber_live
        .subscribe(publisher_addr, BROADCAST_NAME)
        .await?;
    let mut track = subscription.broadcast().video_ready().await?;

    // Let the encoder and the first keyframe settle; measuring the join is a different question.
    tokio::time::sleep(SETTLE).await;

    let origin = started_at
        .lock()
        .unwrap()
        .ok_or_else(|| anyhow::anyhow!("source never started"))?;

    let mut latencies = Vec::new();
    let deadline = Instant::now() + MEASURE_FOR;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), track.next_frame()).await {
            Ok(Some(frame)) => {
                let arrived = origin.elapsed();
                latencies.push(arrived.saturating_sub(frame.timestamp));
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    let delivered = latencies.len();
    latencies.sort();
    Ok(Run {
        latencies,
        delivered,
        source_fps,
    })
}

fn report(run: &Run) {
    let observed_fps = run.delivered as f64 / MEASURE_FOR.as_secs_f64();
    println!(
        "  source {:>4.0} fps -> delivered {:>3} frames ({observed_fps:>5.1} fps)   \
         p50 {:>8.2?}  p90 {:>8.2?}  p99 {:>8.2?}",
        run.source_fps,
        run.delivered,
        percentile(&run.latencies, 0.50),
        percentile(&run.latencies, 0.90),
        percentile(&run.latencies, 0.99),
    );
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    println!(
        "capture -> decoded-frame latency over a real loopback iroh connection\n\
         (measuring {MEASURE_FOR:?} after a {SETTLE:?} settle, {WIDTH}x{HEIGHT})"
    );

    let mut runs = Vec::new();
    for fps in [30.0, 45.0, 60.0, 90.0, 120.0] {
        let run = measure(fps).await?;
        report(&run);
        runs.push(run);
    }
    let thirty = &runs[0];
    let sixty = &runs[2];

    anyhow::ensure!(
        thirty.delivered > 0 && sixty.delivered > 0,
        "no frames were delivered; the harness measured nothing"
    );

    let thirty_fps = thirty.delivered as f64 / MEASURE_FOR.as_secs_f64();
    let sixty_fps = sixty.delivered as f64 / MEASURE_FOR.as_secs_f64();
    println!(
        "\ndoubling the SOURCE rate moved the delivered rate {thirty_fps:.1} -> {sixty_fps:.1} fps"
    );
    if sixty_fps < thirty_fps * 1.3 {
        println!(
            "  -> the pipeline, not the source, is the limit: the encoder loop polls once per \
             33.3ms (moq-media/src/pipeline/video_encode.rs), so a faster source cannot get more \
             frames through. Raising capture fps ALONE would change nothing."
        );
    } else {
        println!("  -> the source rate does carry through, so capture fps is worth raising");
    }

    println!(
        "\np50 latency at 30fps {:.2?}, at 60fps {:.2?}",
        percentile(&thirty.latencies, 0.50),
        percentile(&sixty.latencies, 0.50)
    );
    // Everything above runs against PacedProbeSource, which proves the PRINCIPLE but says
    // nothing about what the daemon actually captures at. This measures the real ScreenCaptureKit
    // capturer through the daemon's own config, so a regression to the default 30fps is caught.
    println!("\nthe daemon's real capturer, at the rate it is actually configured for");
    anyhow::ensure!(
        holoiroh_daemon::capture::CAPTURE_FPS > 30.0,
        "capture is configured at {}fps, back at or below the encoder's own 30Hz poll -- that is \
         the beat this measured at 187ms p50 versus 68ms",
        holoiroh_daemon::capture::CAPTURE_FPS
    );
    match holoiroh_daemon::capture::resolve_display(None) {
        Ok(monitor) => {
            use iroh_live::media::capture::ScreenCapturer;
            let mut capturer = ScreenCapturer::with_monitor_config(
                &monitor,
                &holoiroh_daemon::capture::screen_config(),
            )?;
            capturer.start()?;
            let window = Duration::from_secs(3);
            let started = Instant::now();
            let mut frames = 0u32;
            while started.elapsed() < window {
                if matches!(capturer.pop_frame(), Ok(Some(_))) {
                    frames += 1;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let observed = frames as f64 / window.as_secs_f64();
            println!(
                "  configured {:.0} fps -> observed {observed:.1} fps on {}",
                holoiroh_daemon::capture::CAPTURE_FPS,
                monitor.summary()
            );
            anyhow::ensure!(
                observed > 40.0,
                "the capturer only reached {observed:.1} fps against a configured {:.0}; it is \
                 still beating against the encoder poll",
                holoiroh_daemon::capture::CAPTURE_FPS
            );
        }
        Err(err) => println!(
            "  skipped: no display available ({err:#}). On macOS this is a missing Screen \
             Recording grant, which CI does not have."
        ),
    }

    println!(
        "\nVERDICT: measured. These are the numbers any change to capture rate, encoder pacing, \
         GOP length or playout max_latency has to move; re-run it after each."
    );
    Ok(())
}
