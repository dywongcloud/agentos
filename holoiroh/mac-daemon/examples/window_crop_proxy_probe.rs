use std::collections::BTreeSet;
use std::convert::Infallible;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use base64::Engine;
use futures_util::{StreamExt, stream};
use holoiroh_daemon::local_llama_proxy::LocalLlamaProxy;
use holoiroh_daemon::local_model::DEFAULT_LOCAL_MAX_TOKENS;
use holoiroh_daemon::window_crop::{
    CropSkipReason, DesktopSnapshot, DisplaySnapshot, ScreenRect, SystemWindowSnapshotSource,
    WindowSnapshotSource, crop_enabled_from_env,
};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, Rgb, RgbImage};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct FixedSource(DesktopSnapshot);

impl WindowSnapshotSource for FixedSource {
    fn snapshot(&self) -> Result<DesktopSnapshot, CropSkipReason> {
        Ok(self.0.clone())
    }
}

struct AlternatingSource {
    next: AtomicUsize,
    snapshots: [DesktopSnapshot; 2],
}

impl WindowSnapshotSource for AlternatingSource {
    fn snapshot(&self) -> Result<DesktopSnapshot, CropSkipReason> {
        let index = self.next.fetch_add(1, Ordering::SeqCst) % 2;
        Ok(self.snapshots[index].clone())
    }
}

struct Observed {
    probe_case: String,
    image_dimensions: Option<(u32, u32)>,
    n_predict: Option<u64>,
    cache_prompt: Option<bool>,
    stripped_hop_header: bool,
}

#[derive(Clone)]
struct FakeState {
    observed: mpsc::UnboundedSender<Observed>,
}

fn display(id: u32, x: f64, y: f64, width: f64, height: f64) -> DisplaySnapshot {
    DisplaySnapshot {
        id,
        bounds: ScreenRect::new(x, y, width, height),
    }
}

fn snapshot(window: ScreenRect, displays: Vec<DisplaySnapshot>) -> DesktopSnapshot {
    DesktopSnapshot {
        owner_pid: 42,
        window_bounds: window,
        displays,
    }
}

fn jpeg_data_url(width: u32, height: u32) -> Result<String> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
    });
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 88).encode_image(&DynamicImage::ImageRgb8(image))?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn image_dimensions(value: &Value) -> Option<(u32, u32)> {
    let url = value["messages"][0]["content"]
        .as_array()?
        .iter()
        .find_map(|part| part["image_url"]["url"].as_str())?;
    let encoded = url.split_once(',')?.1;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    image::load_from_memory(&bytes)
        .ok()
        .map(|image| (image.width(), image.height()))
}

fn request_body(probe_case: &str, url: Option<&str>, stream: bool) -> Value {
    let content = match url {
        Some(url) => serde_json::json!([
            {"type":"text","text":"private-probe-sentinel"},
            {"type":"image_url","image_url":{"url":url}}
        ]),
        None => serde_json::json!([{"type":"text","text":"private-probe-sentinel"}]),
    };
    serde_json::json!({
        "probe_case": probe_case,
        "stream": stream,
        "n_predict": 999999,
        "cache_prompt": false,
        "messages": [{"role":"user","content":content}]
    })
}

fn structured(tools: Value) -> String {
    serde_json::json!({"note":null,"thought":"probe","tool_calls":tools}).to_string()
}

fn json_response(content: String) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id":"probe","choices":[{"message":{"role":"assistant","content":content}}]
    }))
    .unwrap()
}

fn response(status: StatusCode, content_type: &'static str, body: Body) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("connection", "keep-alive, x-upstream-hop")
        .header("x-upstream-hop", "secret")
        .body(body)
        .unwrap()
}

async fn fake_upstream(State(state): State<FakeState>, request: Request<Body>) -> Response {
    let stripped_hop_header = !request.headers().contains_key("x-client-hop");
    let bytes = match axum::body::to_bytes(request.into_body(), 64 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return response(StatusCode::BAD_REQUEST, "text/plain", Body::from("bad")),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return response(StatusCode::BAD_REQUEST, "text/plain", Body::from("bad")),
    };
    let probe_case = value["probe_case"].as_str().unwrap_or("unknown").to_owned();
    let dimensions = image_dimensions(&value);
    let _ = state.observed.send(Observed {
        probe_case: probe_case.clone(),
        image_dimensions: dimensions,
        n_predict: value["n_predict"].as_u64(),
        cache_prompt: value["cache_prompt"].as_bool(),
        stripped_hop_header,
    });

    match probe_case.as_str() {
        "coordinates" => {
            let tools = serde_json::json!([
                {"tool_name":"click_desktop","element":"click","x":500,"y":500},
                {"tool_name":"double_click_desktop","element":"double","x":0,"y":0},
                {"tool_name":"move_to_desktop","element":"start","x":250,"y":300},
                {"tool_name":"drag_to_desktop","element":"end","x":750,"y":800},
                {"tool_name":"scroll_desktop","element":"scroll","x":1000,"y":1000,"direction":"down"}
            ]);
            response(
                StatusCode::OK,
                "application/json",
                Body::from(json_response(structured(tools))),
            )
        }
        "text_only" => response(
            StatusCode::OK,
            "application/json",
            Body::from(json_response(structured(serde_json::json!([
                {"tool_name":"answer","content":"text only"}
            ])))),
        ),
        "malformed" => response(
            StatusCode::OK,
            "application/json",
            Body::from(json_response("{".to_owned())),
        ),
        "unknown_tool" => response(
            StatusCode::OK,
            "application/json",
            Body::from(json_response(structured(serde_json::json!([
                {"tool_name":"box_select_desktop","element":"unknown","x":500,"y":500}
            ])))),
        ),
        "incomplete" => response(
            StatusCode::OK,
            "application/json",
            Body::from(json_response(structured(serde_json::json!([
                {"tool_name":"click_desktop","element":"missing y","x":500}
            ])))),
        ),
        "unsupported_type" => response(
            StatusCode::OK,
            "text/plain",
            Body::from(json_response(structured(serde_json::json!([
                {"tool_name":"click_desktop","element":"wrong media type","x":500,"y":500}
            ])))),
        ),
        "oversized" => response(
            StatusCode::OK,
            "application/json",
            Body::from(vec![
                b' ';
                holoiroh_daemon::window_crop::MAX_CROPPED_RESPONSE_BYTES
                    + 1
            ]),
        ),
        "concurrent" => {
            let (x, y) = match dimensions {
                Some((500, 400)) => (500, 500),
                Some((900, 300)) => (500, 500),
                _ => (0, 0),
            };
            response(
                StatusCode::OK,
                "application/json",
                Body::from(json_response(structured(serde_json::json!([
                    {"tool_name":"click_desktop","element":"concurrent","x":x,"y":y}
                ])))),
            )
        }
        "sse_coordinates" => {
            let content = structured(serde_json::json!([
                {"tool_name":"click_desktop","element":"sse","x":500,"y":500}
            ]));
            let midpoint = content.len() / 2;
            let first = serde_json::json!({"choices":[{"delta":{"content":&content[..midpoint]}}]});
            let second =
                serde_json::json!({"choices":[{"delta":{"content":&content[midpoint..]}}]});
            let chunks = vec![
                Ok::<_, Infallible>(Bytes::from(format!("data: {first}\n\n"))),
                Ok(Bytes::from(format!("data: {second}\n\ndata: [DONE]\n\n"))),
            ];
            response(
                StatusCode::OK,
                "text/event-stream",
                Body::from_stream(stream::iter(chunks)),
            )
        }
        "incomplete_sse" => {
            let content = structured(serde_json::json!([
                {"tool_name":"click_desktop","element":"no done","x":500,"y":500}
            ]));
            let event = serde_json::json!({"choices":[{"delta":{"content":content}}]});
            response(
                StatusCode::OK,
                "text/event-stream",
                Body::from(format!("data: {event}\n\n")),
            )
        }
        "timeout" => {
            let chunks = stream::unfold(0, |step| async move {
                match step {
                    0 => Some((Ok::<_, Infallible>(Bytes::from_static(b"data: ")), 1)),
                    1 => {
                        tokio::time::sleep(Duration::from_secs(31)).await;
                        Some((Ok(Bytes::from_static(b"[DONE]\n\n")), 2))
                    }
                    _ => None,
                }
            });
            response(
                StatusCode::OK,
                "text/event-stream",
                Body::from_stream(chunks),
            )
        }
        _ => {
            let chunks = stream::unfold(0, |step| async move {
                match step {
                    0 => Some((Ok::<_, Infallible>(Bytes::from_static(b"first")), 1)),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Some((Ok(Bytes::from_static(b"second")), 2))
                    }
                    _ => None,
                }
            });
            response(
                StatusCode::PARTIAL_CONTENT,
                "application/octet-stream",
                Body::from_stream(chunks),
            )
        }
    }
}

fn extract_structured(body: &[u8], sse: bool) -> Result<Value> {
    let content = if sse {
        let text = std::str::from_utf8(body)?;
        let mut combined = String::new();
        for line in text.lines().filter(|line| line.starts_with("data: ")) {
            let data = &line[6..];
            if data == "[DONE]" {
                continue;
            }
            let event: Value = serde_json::from_str(data)?;
            if let Some(fragment) = event["choices"][0]["delta"]["content"].as_str() {
                combined.push_str(fragment);
            }
        }
        combined
    } else {
        let outer: Value = serde_json::from_slice(body)?;
        outer["choices"][0]["message"]["content"]
            .as_str()
            .context("missing response content")?
            .to_owned()
    };
    serde_json::from_str(&content).context("invalid structured content")
}

async fn post(
    client: &reqwest::Client,
    proxy: &LocalLlamaProxy,
    body: Value,
) -> Result<reqwest::Response> {
    client
        .post(format!("{}/v1/chat/completions", proxy.local_url()))
        .header("connection", "keep-alive, x-client-hop")
        .header("x-client-hop", "private")
        .json(&body)
        .send()
        .await
        .context("proxy request failed")
}

async fn observed(rx: &mut mpsc::UnboundedReceiver<Observed>, expected: &str) -> Result<Observed> {
    let observed = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .context("upstream observation timed out")?
        .context("upstream observation channel closed")?;
    ensure!(observed.probe_case == expected);
    ensure!(observed.n_predict == Some(DEFAULT_LOCAL_MAX_TOKENS as u64));
    ensure!(observed.cache_prompt == Some(true));
    ensure!(observed.stripped_hop_header);
    Ok(observed)
}

async fn assert_streams(
    client: &reqwest::Client,
    proxy: &LocalLlamaProxy,
    body: Value,
) -> Result<()> {
    let response = post(client, proxy, body).await?;
    ensure!(response.status() == StatusCode::PARTIAL_CONTENT);
    let started = Instant::now();
    let mut chunks = response.bytes_stream();
    let first = chunks
        .next()
        .await
        .context("missing first stream chunk")??;
    ensure!(first == "first");
    ensure!(started.elapsed() < Duration::from_millis(100));
    let second = chunks
        .next()
        .await
        .context("missing second stream chunk")??;
    ensure!(second == "second");
    ensure!(started.elapsed() >= Duration::from_millis(200));
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let upstream = listener.local_addr()?;
    let shutdown = CancellationToken::new();
    let serve_shutdown = shutdown.clone();
    let app = axum::Router::new()
        .fallback(axum::routing::any(fake_upstream))
        .with_state(FakeState { observed: tx });
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(serve_shutdown.cancelled_owned())
            .await
            .unwrap();
    });

    let image_url = jpeg_data_url(2000, 1000)?;
    let exact_snapshot = snapshot(
        ScreenRect::new(400.0, 200.0, 800.0, 500.0),
        vec![display(1, 0.0, 0.0, 2000.0, 1000.0)],
    );
    let proxy = LocalLlamaProxy::spawn_for_loopback_upstream_with_crop_source(
        upstream,
        DEFAULT_LOCAL_MAX_TOKENS,
        Arc::new(FixedSource(exact_snapshot.clone())),
    )
    .await?;
    let client = reqwest::Client::new();

    let response = post(
        &client,
        &proxy,
        request_body("coordinates", Some(&image_url), false),
    )
    .await?;
    ensure!(response.status() == StatusCode::OK);
    ensure!(!response.headers().contains_key("x-upstream-hop"));
    let declared_length = response
        .content_length()
        .context("missing rewritten content length")?;
    let bytes = response.bytes().await?;
    ensure!(declared_length == bytes.len() as u64);
    let output = extract_structured(&bytes, false)?;
    let tools = output["tool_calls"].as_array().unwrap();
    ensure!((tools[0]["x"].as_i64(), tools[0]["y"].as_i64()) == (Some(400), Some(450)));
    ensure!((tools[1]["x"].as_i64(), tools[1]["y"].as_i64()) == (Some(200), Some(200)));
    ensure!((tools[2]["x"].as_i64(), tools[2]["y"].as_i64()) == (Some(300), Some(350)));
    ensure!((tools[3]["x"].as_i64(), tools[3]["y"].as_i64()) == (Some(500), Some(600)));
    ensure!((tools[4]["x"].as_i64(), tools[4]["y"].as_i64()) == (Some(600), Some(700)));
    ensure!(observed(&mut rx, "coordinates").await?.image_dimensions == Some((800, 500)));

    let expected_text = json_response(structured(serde_json::json!([
        {"tool_name":"answer","content":"text only"}
    ])));
    let response = post(
        &client,
        &proxy,
        request_body("text_only", Some(&image_url), false),
    )
    .await?;
    ensure!(response.bytes().await?.as_ref() == expected_text.as_slice());
    ensure!(observed(&mut rx, "text_only").await?.image_dimensions == Some((800, 500)));

    let response = post(
        &client,
        &proxy,
        request_body("sse_coordinates", Some(&image_url), true),
    )
    .await?;
    let output = extract_structured(&response.bytes().await?, true)?;
    ensure!(
        (
            output["tool_calls"][0]["x"].as_i64(),
            output["tool_calls"][0]["y"].as_i64()
        ) == (Some(400), Some(450))
    );
    ensure!(observed(&mut rx, "sse_coordinates").await?.image_dimensions == Some((800, 500)));

    for case in [
        "malformed",
        "unknown_tool",
        "incomplete",
        "unsupported_type",
        "incomplete_sse",
        "oversized",
    ] {
        let response = post(&client, &proxy, request_body(case, Some(&image_url), false)).await?;
        ensure!(response.status() == StatusCode::BAD_GATEWAY);
        let error_body = response.text().await?;
        ensure!(!error_body.contains("private-probe-sentinel"));
        ensure!(observed(&mut rx, case).await?.image_dimensions == Some((800, 500)));
    }

    let timeout_started = Instant::now();
    let response = post(
        &client,
        &proxy,
        request_body("timeout", Some(&image_url), true),
    )
    .await?;
    ensure!(response.status() == StatusCode::GATEWAY_TIMEOUT);
    ensure!(timeout_started.elapsed() >= Duration::from_secs(30));
    ensure!(!response.text().await?.contains("private-probe-sentinel"));
    ensure!(observed(&mut rx, "timeout").await?.image_dimensions == Some((800, 500)));

    assert_streams(&client, &proxy, request_body("no_image_stream", None, true)).await?;
    ensure!(
        observed(&mut rx, "no_image_stream")
            .await?
            .image_dimensions
            .is_none()
    );

    let ambiguous_snapshot = snapshot(
        exact_snapshot.window_bounds,
        vec![
            display(1, 0.0, 0.0, 2000.0, 1000.0),
            display(2, 2000.0, 0.0, 2000.0, 1000.0),
        ],
    );
    let ambiguous_proxy = LocalLlamaProxy::spawn_for_loopback_upstream_with_crop_source(
        upstream,
        DEFAULT_LOCAL_MAX_TOKENS,
        Arc::new(FixedSource(ambiguous_snapshot)),
    )
    .await?;
    assert_streams(
        &client,
        &ambiguous_proxy,
        request_body("ambiguous_stream", Some(&image_url), true),
    )
    .await?;
    ensure!(
        observed(&mut rx, "ambiguous_stream")
            .await?
            .image_dimensions
            == Some((2000, 1000))
    );

    let multidisplay_snapshot = snapshot(
        exact_snapshot.window_bounds,
        vec![
            display(1, 0.0, 0.0, 2000.0, 1000.0),
            display(2, 2000.0, 0.0, 1000.0, 1000.0),
        ],
    );
    let multidisplay_proxy = LocalLlamaProxy::spawn_for_loopback_upstream_with_crop_source(
        upstream,
        DEFAULT_LOCAL_MAX_TOKENS,
        Arc::new(FixedSource(multidisplay_snapshot)),
    )
    .await?;
    assert_streams(
        &client,
        &multidisplay_proxy,
        request_body("multidisplay_stream", Some(&image_url), true),
    )
    .await?;
    ensure!(
        observed(&mut rx, "multidisplay_stream")
            .await?
            .image_dimensions
            == Some((2000, 1000))
    );

    let spanning_snapshot = snapshot(
        ScreenRect::new(1800.0, 200.0, 400.0, 400.0),
        vec![
            display(1, 0.0, 0.0, 2000.0, 1000.0),
            display(2, 2000.0, 0.0, 1000.0, 1000.0),
        ],
    );
    let spanning_proxy = LocalLlamaProxy::spawn_for_loopback_upstream_with_crop_source(
        upstream,
        DEFAULT_LOCAL_MAX_TOKENS,
        Arc::new(FixedSource(spanning_snapshot)),
    )
    .await?;
    assert_streams(
        &client,
        &spanning_proxy,
        request_body("spanning_stream", Some(&image_url), true),
    )
    .await?;
    ensure!(observed(&mut rx, "spanning_stream").await?.image_dimensions == Some((2000, 1000)));

    ensure!(crop_enabled_from_env());
    let live_snapshot = SystemWindowSnapshotSource
        .snapshot()
        .map_err(|reason| anyhow::anyhow!("live source failed: {}", reason.code()))?;
    ensure!(live_snapshot.displays.len() == 1);
    let live_full = (
        live_snapshot.displays[0].bounds.width.round() as u32,
        live_snapshot.displays[0].bounds.height.round() as u32,
    );
    let live_url = jpeg_data_url(live_full.0, live_full.1)?;
    let production_proxy =
        LocalLlamaProxy::spawn_for_loopback_upstream(upstream, DEFAULT_LOCAL_MAX_TOKENS).await?;
    let response = post(
        &client,
        &production_proxy,
        request_body("text_only", Some(&live_url), false),
    )
    .await?;
    ensure!(response.bytes().await?.as_ref() == expected_text.as_slice());
    let live_crop = observed(&mut rx, "text_only")
        .await?
        .image_dimensions
        .context("production source did not forward an image")?;
    ensure!(live_crop.0 < live_full.0 || live_crop.1 < live_full.1);

    let concurrent_source = AlternatingSource {
        next: AtomicUsize::new(0),
        snapshots: [
            snapshot(
                ScreenRect::new(100.0, 100.0, 500.0, 400.0),
                vec![display(1, 0.0, 0.0, 2000.0, 1000.0)],
            ),
            snapshot(
                ScreenRect::new(700.0, 500.0, 900.0, 300.0),
                vec![display(1, 0.0, 0.0, 2000.0, 1000.0)],
            ),
        ],
    };
    let concurrent_proxy = LocalLlamaProxy::spawn_for_loopback_upstream_with_crop_source(
        upstream,
        DEFAULT_LOCAL_MAX_TOKENS,
        Arc::new(concurrent_source),
    )
    .await?;
    let request_a = post(
        &client,
        &concurrent_proxy,
        request_body("concurrent", Some(&image_url), false),
    );
    let request_b = post(
        &client,
        &concurrent_proxy,
        request_body("concurrent", Some(&image_url), false),
    );
    let (response_a, response_b) = tokio::join!(request_a, request_b);
    let output_a = extract_structured(&response_a?.bytes().await?, false)?;
    let output_b = extract_structured(&response_b?.bytes().await?, false)?;
    let mut coordinates = BTreeSet::new();
    for output in [output_a, output_b] {
        coordinates.insert((
            output["tool_calls"][0]["x"].as_i64().unwrap(),
            output["tool_calls"][0]["y"].as_i64().unwrap(),
        ));
    }
    ensure!(coordinates == BTreeSet::from([(175, 300), (575, 650)]));
    let mut dimensions = BTreeSet::new();
    dimensions.insert(
        observed(&mut rx, "concurrent")
            .await?
            .image_dimensions
            .unwrap(),
    );
    dimensions.insert(
        observed(&mut rx, "concurrent")
            .await?
            .image_dimensions
            .unwrap(),
    );
    ensure!(dimensions == BTreeSet::from([(500, 400), (900, 300)]));

    concurrent_proxy.shutdown().await?;
    production_proxy.shutdown().await?;
    spanning_proxy.shutdown().await?;
    multidisplay_proxy.shutdown().await?;
    ambiguous_proxy.shutdown().await?;
    proxy.shutdown().await?;
    shutdown.cancel();
    task.await?;

    println!("fake_upstream_crop=2000x1000_to_800x500 click_500_500=400,450");
    println!(
        "coordinate_tools=all drag_endpoints=rebased text_only=byte_exact sse=buffered_rebased"
    );
    println!("fallback_streaming=no_image,ambiguous_display,all_multidisplay,multidisplay_span");
    println!("production_default=enabled live_source=request_time_crop");
    println!(
        "fail_closed=malformed,unknown_tool,incomplete,unsupported_media,incomplete_sse,oversized,timeout concurrency=request_local"
    );
    println!("WINDOW CROP PROXY PROBE PASSED");
    Ok(())
}
