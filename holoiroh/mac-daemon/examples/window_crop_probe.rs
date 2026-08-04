use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use base64::Engine;
use holoiroh_daemon::window_crop::{
    CropSkipReason, DesktopSnapshot, DisplaySnapshot, PixelRect, ScreenRect, WindowSnapshotSource,
    crop_chat_request, rebase_response, resolve_crop,
};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, Rgb, RgbImage};
use serde_json::Value;

#[derive(Clone)]
struct FixedSource(DesktopSnapshot);

impl WindowSnapshotSource for FixedSource {
    fn snapshot(&self) -> Result<DesktopSnapshot, CropSkipReason> {
        Ok(self.0.clone())
    }
}

fn snapshot(window: ScreenRect, displays: Vec<DisplaySnapshot>) -> DesktopSnapshot {
    DesktopSnapshot {
        owner_pid: 42,
        window_bounds: window,
        displays,
    }
}

fn display(id: u32, x: f64, y: f64, width: f64, height: f64) -> DisplaySnapshot {
    DisplaySnapshot {
        id,
        bounds: ScreenRect::new(x, y, width, height),
    }
}

fn jpeg_data_url(width: u32, height: u32) -> Result<(String, usize)> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            (x % 251) as u8,
            (y % 241) as u8,
            ((x.wrapping_add(y)) % 239) as u8,
        ])
    });
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 88).encode_image(&DynamicImage::ImageRgb8(image))?;
    Ok((
        format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ),
        bytes.len(),
    ))
}

fn request(url: String) -> Value {
    serde_json::json!({
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "probe"},
                {"type": "image_url", "image_url": {"url": url}}
            ]
        }]
    })
}

fn percentile(values: &mut [Duration], fraction: f64) -> Duration {
    values.sort_unstable();
    let index = ((values.len() - 1) as f64 * fraction).round() as usize;
    values[index]
}

fn main() -> Result<()> {
    let one_display = vec![display(1, 0.0, 0.0, 2000.0, 1000.0)];
    let exact = snapshot(
        ScreenRect::new(400.0, 200.0, 800.0, 500.0),
        one_display.clone(),
    );
    let crop = resolve_crop(&exact, 2000, 1000)
        .map_err(|reason| anyhow::anyhow!("exact crop resolution failed: {}", reason.code()))?;
    ensure!(
        crop == PixelRect {
            x: 400,
            y: 200,
            width: 800,
            height: 500
        }
    );

    let transform = holoiroh_daemon::window_crop::CropTransform {
        full_width: 2000,
        full_height: 1000,
        crop,
    };
    ensure!(transform.matrix() == [[0.4, 0.0, 200.0], [0.0, 0.5, 200.0], [0.0, 0.0, 1.0]]);
    ensure!(transform.rebase(500, 500)? == (400, 450));

    let structured = serde_json::json!({
        "note": "unchanged note",
        "thought": "unchanged thought",
        "tool_calls": [
            {"tool_name":"click_desktop","element":"click","x":500,"y":500},
            {"tool_name":"double_click_desktop","element":"double","x":0,"y":0},
            {"tool_name":"move_to_desktop","element":"drag start","x":250,"y":300},
            {"tool_name":"drag_to_desktop","element":"drag end","x":750,"y":800},
            {"tool_name":"scroll_desktop","element":"scroll","x":1000,"y":1000,"direction":"down"},
            {"tool_name":"write_desktop","content":"text-only","press_enter":false},
            {"tool_name":"answer","content":"done"}
        ]
    });
    let response = serde_json::json!({
        "choices": [{"message": {"role":"assistant", "content": structured.to_string()}}]
    });
    let rebased = rebase_response(
        &serde_json::to_vec(&response)?,
        Some("application/json"),
        transform,
    )?;
    ensure!(rebased.coordinate_count == 10);
    let parsed: Value = serde_json::from_slice(&rebased.bytes)?;
    let output: Value =
        serde_json::from_str(parsed["choices"][0]["message"]["content"].as_str().unwrap())?;
    let tools = output["tool_calls"].as_array().unwrap();
    ensure!((tools[0]["x"].as_i64(), tools[0]["y"].as_i64()) == (Some(400), Some(450)));
    ensure!((tools[1]["x"].as_i64(), tools[1]["y"].as_i64()) == (Some(200), Some(200)));
    ensure!((tools[2]["x"].as_i64(), tools[2]["y"].as_i64()) == (Some(300), Some(350)));
    ensure!((tools[3]["x"].as_i64(), tools[3]["y"].as_i64()) == (Some(500), Some(600)));
    ensure!((tools[4]["x"].as_i64(), tools[4]["y"].as_i64()) == (Some(600), Some(700)));
    ensure!(tools[5]["content"] == "text-only");
    ensure!(output["note"] == "unchanged note" && output["thought"] == "unchanged thought");

    let ambiguous = snapshot(
        exact.window_bounds,
        vec![
            display(1, 0.0, 0.0, 2000.0, 1000.0),
            display(2, 2000.0, 0.0, 2000.0, 1000.0),
        ],
    );
    ensure!(resolve_crop(&ambiguous, 2000, 1000) == Err(CropSkipReason::DisplayAmbiguous));
    let unique_multidisplay = snapshot(
        exact.window_bounds,
        vec![
            display(1, 0.0, 0.0, 2000.0, 1000.0),
            display(2, 2000.0, 0.0, 1000.0, 1000.0),
        ],
    );
    ensure!(
        resolve_crop(&unique_multidisplay, 2000, 1000) == Err(CropSkipReason::DisplayAmbiguous)
    );
    let spanning = snapshot(
        ScreenRect::new(1800.0, 200.0, 400.0, 400.0),
        vec![
            display(1, 0.0, 0.0, 2000.0, 1000.0),
            display(2, 2000.0, 0.0, 1000.0, 1000.0),
        ],
    );
    ensure!(resolve_crop(&spanning, 2000, 1000) == Err(CropSkipReason::WindowSpansDisplays));
    let mismatch = snapshot(
        exact.window_bounds,
        vec![display(1, 0.0, 0.0, 1600.0, 1200.0)],
    );
    ensure!(resolve_crop(&mismatch, 2000, 1000) == Err(CropSkipReason::DisplayMismatch));
    let clamped = snapshot(
        ScreenRect::new(-100.0, 100.0, 600.0, 400.0),
        one_display.clone(),
    );
    ensure!(
        resolve_crop(&clamped, 2000, 1000).map_err(|reason| anyhow::anyhow!(
            "clamped crop resolution failed: {}",
            reason.code()
        ))? == PixelRect {
            x: 0,
            y: 100,
            width: 500,
            height: 400
        }
    );
    ensure!(
        resolve_crop(&snapshot(exact.window_bounds, Vec::new()), 2000, 1000)
            == Err(CropSkipReason::DisplayList)
    );

    let (url, source_jpeg_bytes) = jpeg_data_url(2000, 1000)?;
    let source = Arc::new(FixedSource(exact));
    let mut resolver_latencies = Vec::new();
    for _ in 0..2000 {
        let started = Instant::now();
        ensure!(resolve_crop(&source.0, 2000, 1000).is_ok());
        resolver_latencies.push(started.elapsed());
    }

    let mut decode_latencies = Vec::new();
    let mut encode_latencies = Vec::new();
    let mut total_latencies = Vec::new();
    let mut last_metadata = None;
    for _ in 0..20 {
        let mut body = request(url.clone());
        let started = Instant::now();
        let outcome = crop_chat_request(&mut body, source.as_ref());
        total_latencies.push(started.elapsed());
        let metadata = outcome.metadata.unwrap();
        decode_latencies.push(metadata.decode_latency);
        encode_latencies.push(metadata.encode_latency);
        ensure!(metadata.crop_width == 800 && metadata.crop_height == 500);
        last_metadata = Some(metadata);
    }
    let metadata = last_metadata.unwrap();
    ensure!(metadata.original_jpeg_bytes == source_jpeg_bytes);

    println!(
        "schema=NoteMultiStructuredOutput coordinate_tools=click_desktop,double_click_desktop,drag_to_desktop,scroll_desktop,move_to_desktop"
    );
    println!(
        "matrix=[[0.4,0,200],[0,0.5,200],[0,0,1]] click_500_500=400,450 drag_start_250_300=300,350 drag_end_750_800=500,600"
    );
    println!(
        "resolver_cases=single_ok,missing_rejected,ambiguous_rejected,all_multidisplay_rejected,spanning_rejected,aspect_rejected,clamped_ok"
    );
    println!(
        "dimensions source=2000x1000 crop=800x500 jpeg_bytes_source={} jpeg_bytes_crop={}",
        metadata.original_jpeg_bytes, metadata.cropped_jpeg_bytes
    );
    println!(
        "latency repetitions resolver=2000 crop_pipeline=20 resolver_p50_ns={} resolver_p95_ns={} decode_p50_ms={:.3} decode_p95_ms={:.3} encode_p50_ms={:.3} encode_p95_ms={:.3} total_p50_ms={:.3} total_p95_ms={:.3}",
        percentile(&mut resolver_latencies, 0.50).as_nanos(),
        percentile(&mut resolver_latencies, 0.95).as_nanos(),
        percentile(&mut decode_latencies, 0.50).as_secs_f64() * 1000.0,
        percentile(&mut decode_latencies, 0.95).as_secs_f64() * 1000.0,
        percentile(&mut encode_latencies, 0.50).as_secs_f64() * 1000.0,
        percentile(&mut encode_latencies, 0.95).as_secs_f64() * 1000.0,
        percentile(&mut total_latencies, 0.50).as_secs_f64() * 1000.0,
        percentile(&mut total_latencies, 0.95).as_secs_f64() * 1000.0,
    );
    println!("WINDOW CROP RESOLVER PROBE PASSED");
    Ok(())
}
