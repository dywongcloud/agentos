use screencapturekit::async_api::{AsyncSCShareableContent, AsyncSCStream};
use screencapturekit::prelude::*;
use std::error::Error;
use std::time::{Duration, Instant};

fn config(display: &SCDisplay) -> SCStreamConfiguration {
    SCStreamConfiguration::new()
        .with_width(display.width())
        .with_height(display.height())
        .with_pixel_format(PixelFormat::BGRA)
        .with_fps(60)
        .with_queue_depth(8)
        .with_shows_cursor(true)
}

async fn make_stream() -> Result<AsyncSCStream, Box<dyn Error>> {
    let content = AsyncSCShareableContent::get().await?;
    let display = content
        .displays()
        .into_iter()
        .next()
        .ok_or("no display is available")?;
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build();
    Ok(AsyncSCStream::new(
        &filter,
        &config(&display),
        8,
        SCStreamOutputType::Screen,
    ))
}

fn drain_for(stream: &AsyncSCStream, duration: Duration) -> u64 {
    let deadline = Instant::now() + duration;
    let mut count = 0u64;
    while Instant::now() < deadline {
        if stream.try_next().is_some() {
            count += 1;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    count
}

async fn run() -> Result<(), Box<dyn Error>> {
    let content_started = Instant::now();
    let content = AsyncSCShareableContent::get().await?;
    let content_elapsed = content_started.elapsed();
    println!("operation=async_api");
    println!("permission_context=allowed");
    println!("display_count={}", content.displays().len());
    println!("window_count={}", content.windows().len());
    println!(
        "async_enumeration_ms={:.3}",
        content_elapsed.as_secs_f64() * 1000.0
    );

    let stream = make_stream().await?;
    let start_started = Instant::now();
    stream.start_capture().await?;
    let start_elapsed = start_started.elapsed();
    let frames = drain_for(&stream, Duration::from_secs(1));
    stream.clear_buffer();
    let stop_started = Instant::now();
    stream.stop_capture().await?;
    let stop_elapsed = stop_started.elapsed();
    let frames_after_stop = drain_for(&stream, Duration::from_millis(500));
    println!(
        "awaited_start_ms={:.3}",
        start_elapsed.as_secs_f64() * 1000.0
    );
    println!("frames_before_stop={frames}");
    println!("awaited_stop_ms={:.3}", stop_elapsed.as_secs_f64() * 1000.0);
    println!("frames_after_stop={frames_after_stop}");
    drop(stream);

    let hot_future_stream = make_stream().await?;
    hot_future_stream.start_capture().await?;
    drain_for(&hot_future_stream, Duration::from_millis(250));
    hot_future_stream.clear_buffer();
    let hot_stop = hot_future_stream.stop_capture();
    drop(hot_stop);
    let frames_after_dropped_stop_future =
        drain_for(&hot_future_stream, Duration::from_millis(500));
    println!("dropped_stop_future=true");
    println!("frames_after_dropped_stop_future={frames_after_dropped_stop_future}");
    drop(hot_future_stream);

    let dropped_stream = make_stream().await?;
    dropped_stream.start_capture().await?;
    drain_for(&dropped_stream, Duration::from_millis(250));
    let drop_started = Instant::now();
    drop(dropped_stream);
    let drop_elapsed = drop_started.elapsed();
    std::thread::sleep(Duration::from_millis(500));
    println!(
        "async_stream_drop_ms={:.3}",
        drop_elapsed.as_secs_f64() * 1000.0
    );
    println!("process_exit=clean");
    Ok(())
}

fn main() {
    if let Err(error) = futures_lite::future::block_on(run()) {
        eprintln!("result=error");
        eprintln!("error={error:?}");
        std::process::exit(1);
    }
}
