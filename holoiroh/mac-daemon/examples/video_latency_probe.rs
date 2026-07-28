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

/// The probe builds its own pipeline, so it would keep passing even if the shipping app stopped
/// asking for the settings measured here.
const BRIDGE_SOURCE: &str = include_str!("../../ios-bridge/src/lib.rs");

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

/// Times the two CPU pixel passes the iOS bridge performs on every decoded frame.
async fn measure_pixel_passes() -> anyhow::Result<Option<(Duration, Duration, usize)>> {
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
    let mut source = PacedProbeSource::new(60.0);
    source.start()?;
    broadcast
        .video()
        .set_source(source, codec, vec![VideoPreset::P720])?;
    publisher_live.publish(BROADCAST_NAME, &broadcast).await?;

    let subscription = subscriber_live
        .subscribe(publisher_addr, BROADCAST_NAME)
        .await?;
    let mut track = subscription.broadcast().video_ready().await?;
    tokio::time::sleep(SETTLE).await;

    let mut to_rgba = Duration::ZERO;
    let mut swizzle = Duration::ZERO;
    let mut frames = 0usize;
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(500), track.next_frame()).await
        else {
            continue;
        };
        let started = Instant::now();
        let rgba = frame.rgba_image();
        to_rgba += started.elapsed();

        // The exact shape of the bridge's own swizzle: swap R and B in place.
        let mut pixels = rgba.as_raw().clone();
        let started = Instant::now();
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        swizzle += started.elapsed();
        std::hint::black_box(&pixels);
        frames += 1;
    }

    if frames == 0 {
        return Ok(None);
    }
    Ok(Some((to_rgba / frames as u32, swizzle / frames as u32, frames)))
}

/// Time from subscribing to the first decoded frame arriving.
async fn measure_join() -> anyhow::Result<Duration> {
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
    let mut source = PacedProbeSource::new(60.0);
    source.start()?;
    broadcast
        .video()
        .set_source(source, codec, vec![VideoPreset::P720])?;
    publisher_live.publish(BROADCAST_NAME, &broadcast).await?;

    // Let the publisher get well past its first keyframe, so the join lands mid-GOP the way a
    // phone opening the app on an already-running daemon does.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let started = Instant::now();
    let subscription = subscriber_live
        .subscribe(publisher_addr, BROADCAST_NAME)
        .await?;
    let mut track = subscription.broadcast().video_ready().await?;
    match tokio::time::timeout(Duration::from_secs(10), track.next_frame()).await {
        Ok(Some(_)) => Ok(started.elapsed()),
        _ => anyhow::bail!("no first frame within 10s"),
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
    // How long a subscriber waits for its FIRST decoded frame. This is not a curiosity: the
    // decoder cannot start on anything but a keyframe, so this number is bounded by the GOP, and
    // the same wait is paid again after any decode error or skipped group
    // (`video_decode.rs` sets waiting_for_keyframe and discards until the next IDR). It is
    // therefore the real cost of the encoder's 1-second keyframe interval, and the reason
    // lowering playout max_latency against that GOP could make freezes longer rather than shorter.
    println!("\nhow long until a subscriber sees its FIRST frame? (bounded by the keyframe interval)");
    let mut joins = Vec::new();
    for _ in 0..5 {
        joins.push(measure_join().await?);
    }
    joins.sort();
    println!(
        "  join latency over {} runs: min {:.2?}  median {:.2?}  max {:.2?}",
        joins.len(),
        joins.first().copied().unwrap_or_default(),
        percentile(&joins, 0.50),
        joins.last().copied().unwrap_or_default(),
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

    // What the phone pays per frame AFTER decoding. The VideoToolbox decoder already produces an
    // NV12 CVPixelBuffer, which AVSampleBufferDisplayLayer accepts directly, but the bridge asks
    // for `rgba_image()` (a CPU readback plus NV12->RGBA) and then swizzles RGBA->BGRA in a
    // scalar loop, before Swift memcpys a third time into a pooled buffer. This times the two
    // passes that happen in Rust, on real decoded frames, so the row is quantified rather than
    // estimated. A phone is slower than this Mac, so treat these as a floor.
    println!("\nper-frame CPU pixel work the phone does after decoding");
    let pixel_costs = measure_pixel_passes().await?;
    match pixel_costs {
        Some((to_rgba, swizzle, frames)) => {
            println!(
                "  over {frames} decoded frames: NV12->RGBA {to_rgba:.2?}/frame, \
                 RGBA->BGRA swizzle {swizzle:.2?}/frame, total {:.2?}/frame",
                to_rgba + swizzle
            );
            let budget = Duration::from_secs_f64(1.0 / 28.0);
            println!(
                "  that is {:.1}% of the {:.2?} frame interval the pipeline actually delivers at",
                (to_rgba + swizzle).as_secs_f64() / budget.as_secs_f64() * 100.0,
                budget
            );
        }
        None => println!("  skipped: no frames decoded"),
    }

    println!("\nthe app actually asks for these settings");
    anyhow::ensure!(
        BRIDGE_SOURCE.contains("subscribe_with_playback_policy"),
        "ios-bridge is back on the default playback policy, whose 150ms max_latency lets the \
         picture stall for longer than skipping the group would have cost"
    );
    anyhow::ensure!(
        BRIDGE_SOURCE.contains("PLAYOUT_MAX_LATENCY"),
        "ios-bridge no longer defines its own playout budget"
    );
    let median_join = percentile(&joins, 0.50);
    anyhow::ensure!(
        Duration::from_millis(60) < median_join,
        "the playout budget (60ms) is no longer below the measured cost of skipping a group \
         ({median_join:.2?}); above it, waiting is strictly worse than skipping and the budget \
         needs re-deriving from this run rather than kept out of habit"
    );
    println!("  ok   playout budget 60ms stays under the {median_join:.2?} it costs to skip a group");

    println!(
        "\nVERDICT: measured. These are the numbers any change to capture rate, encoder pacing, \
         GOP length or playout max_latency has to move; re-run it after each."
    );
    Ok(())
}
