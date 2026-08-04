use std::convert::Infallible;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use futures_util::{StreamExt, stream};
use holoiroh_daemon::local_llama_proxy::{LocalLlamaProxy, MAX_REQUEST_BODY_BYTES};
use holoiroh_daemon::local_model::{
    DEFAULT_LOCAL_MAX_TOKENS, LocalModelConfig, local_max_tokens_from_value,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("log capture lock poisoned"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct ObservedRequest {
    path: String,
    json: Option<Value>,
    authorization: Option<String>,
    content_type: Option<String>,
    hop_sentinel_present: bool,
}

#[derive(Clone)]
struct FakeState {
    observed: mpsc::UnboundedSender<ObservedRequest>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let diagnostics_writer = diagnostics.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || CaptureWriter(diagnostics_writer.clone()))
        .try_init()
        .map_err(|err| anyhow::anyhow!("installing probe tracing subscriber: {err}"))?;

    witness_config_bounds()?;
    witness_argv_guard()?;

    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let fake_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let fake_addr = fake_listener.local_addr()?;
    let fake_shutdown = CancellationToken::new();
    let fake_shutdown_task = fake_shutdown.clone();
    let fake_app = axum::Router::new()
        .fallback(axum::routing::any(fake_upstream))
        .with_state(FakeState {
            observed: observed_tx,
        });
    let fake_task = tokio::spawn(async move {
        axum::serve(fake_listener, fake_app)
            .with_graceful_shutdown(fake_shutdown_task.cancelled_owned())
            .await
    });

    let proxy =
        LocalLlamaProxy::spawn_for_loopback_upstream(fake_addr, DEFAULT_LOCAL_MAX_TOKENS).await?;
    let client = reqwest::Client::new();
    let chat_url = format!("{}/v1/chat/completions", proxy.local_url());
    let models_url = format!("{}/v1/models?probe=1", proxy.local_url());

    let rewrite_response = client
        .post(&chat_url)
        .header("authorization", "Bearer probe-key")
        .header("content-type", "application/json")
        .header("connection", "x-hop-sentinel")
        .header("x-hop-sentinel", "must-not-forward")
        .json(&serde_json::json!({
            "model": "holo-probe",
            "max_completion_tokens": 2048,
            "n_predict": 9999,
            "messages": [{"role": "user", "content": "PROMPT_RAW_SENTINEL_7f91"}]
        }))
        .send()
        .await?;
    ensure!(rewrite_response.status() == StatusCode::CREATED);
    ensure!(
        rewrite_response
            .headers()
            .get("x-upstream-witness")
            .and_then(|v| v.to_str().ok())
            == Some("json")
    );
    ensure!(rewrite_response.json::<Value>().await? == serde_json::json!({"ok": true}));
    let observed = recv_observed(&mut observed_rx).await?;
    let json = observed
        .json
        .context("fake upstream did not receive JSON")?;
    ensure!(observed.path == "/v1/chat/completions");
    ensure!(json["n_predict"] == serde_json::json!(512));
    ensure!(json["cache_prompt"] == serde_json::json!(true));
    ensure!(json["max_completion_tokens"] == serde_json::json!(2048));
    ensure!(observed.authorization.as_deref() == Some("Bearer probe-key"));
    ensure!(observed.content_type.as_deref() == Some("application/json"));
    ensure!(!observed.hop_sentinel_present);
    println!(
        "WITNESS rewrite: caller max_completion_tokens=2048 n_predict=9999 -> upstream n_predict=512 cache_prompt=true"
    );
    println!(
        "WITNESS headers: authorization/content-type preserved; Connection-nominated header stripped"
    );

    let malformed = client.post(&chat_url).body("{").send().await?;
    ensure!(malformed.status() == StatusCode::BAD_REQUEST);
    ensure_no_upstream(&mut observed_rx, "malformed chat JSON").await?;
    println!("WITNESS malformed: status=400 upstream_requests=0");

    let oversized = client
        .post(&chat_url)
        .body(vec![b'x'; MAX_REQUEST_BODY_BYTES + 1])
        .send()
        .await?;
    ensure!(oversized.status() == StatusCode::PAYLOAD_TOO_LARGE);
    ensure_no_upstream(&mut observed_rx, "oversized chat JSON").await?;
    println!(
        "WITNESS oversized: bytes={} status=413 upstream_requests=0",
        MAX_REQUEST_BODY_BYTES + 1
    );

    let models = client
        .get(&models_url)
        .header("authorization", "Bearer models-key")
        .send()
        .await?;
    ensure!(models.status() == StatusCode::OK);
    ensure!(models.json::<Value>().await? == serde_json::json!({"data": []}));
    let observed = recv_observed(&mut observed_rx).await?;
    ensure!(observed.path == "/v1/models?probe=1");
    ensure!(observed.authorization.as_deref() == Some("Bearer models-key"));
    println!("WITNESS non-chat: GET /v1/models query/status/body passed through");

    let stream_started = Instant::now();
    let stream_response = client
        .post(&chat_url)
        .json(&serde_json::json!({
            "stream": true,
            "max_completion_tokens": 2048,
            "messages": []
        }))
        .send()
        .await?;
    ensure!(stream_response.status() == StatusCode::PARTIAL_CONTENT);
    ensure!(
        stream_response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            == Some("text/event-stream")
    );
    ensure!(
        stream_response
            .headers()
            .get("x-upstream-witness")
            .and_then(|v| v.to_str().ok())
            == Some("sse")
    );
    let observed = recv_observed(&mut observed_rx).await?;
    let json = observed.json.context("stream request was not JSON")?;
    ensure!(json["n_predict"] == serde_json::json!(512));
    ensure!(json["cache_prompt"] == serde_json::json!(true));

    let mut chunks = stream_response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(1), chunks.next())
        .await
        .context("first SSE chunk deadline")?
        .context("first SSE chunk missing")??;
    let first_elapsed = stream_started.elapsed();
    let second = tokio::time::timeout(Duration::from_secs(1), chunks.next())
        .await
        .context("second SSE chunk deadline")?
        .context("second SSE chunk missing")??;
    ensure!(first == Bytes::from_static(b"data: {\"step\":1}\n\n"));
    ensure!(second == Bytes::from_static(b"data: [DONE]\n\n"));
    ensure!(first_elapsed < Duration::from_millis(200));
    println!(
        "WITNESS SSE: status=206 content-type=text/event-stream chunk_lengths=[{},{}] first_chunk_ms={}",
        first.len(),
        second.len(),
        first_elapsed.as_millis()
    );

    let non_loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 8080);
    ensure!(
        LocalLlamaProxy::spawn_for_loopback_upstream(non_loopback, 512)
            .await
            .is_err()
    );
    println!("WITNESS confinement: non-loopback upstream rejected");

    proxy.shutdown().await?;
    fake_shutdown.cancel();
    fake_task.await??;

    let captured = diagnostics
        .lock()
        .map_err(|_| anyhow::anyhow!("log capture lock poisoned"))?;
    let diagnostics_text = String::from_utf8_lossy(&captured);
    ensure!(!diagnostics_text.contains("PROMPT_RAW_SENTINEL_7f91"));
    println!("WITNESS diagnostics: raw prompt sentinel absent");
    println!("LOCAL LLAMA PROXY PROBE PASSED");
    Ok(())
}

fn witness_config_bounds() -> Result<()> {
    ensure!(local_max_tokens_from_value(None) == 512);
    ensure!(local_max_tokens_from_value(Some("1024")) == 1024);
    ensure!(local_max_tokens_from_value(Some("0")) == 512);
    ensure!(local_max_tokens_from_value(Some("2049")) == 512);
    ensure!(local_max_tokens_from_value(Some("not-a-number")) == 512);
    println!("WITNESS token env: default=512 valid=1024 zero/overflow/malformed=>512");
    Ok(())
}

fn witness_argv_guard() -> Result<()> {
    let args = LocalModelConfig::default().command_args();
    for forbidden in [
        "--cache-reuse",
        "--cache-ram",
        "--ubatch-size",
        "-ub",
        "--kv-unified",
    ] {
        ensure!(!args.iter().any(|arg| arg == forbidden));
    }
    println!("WITNESS argv: no cache-reuse/cache-ram/ubatch/KV tuning flags");
    Ok(())
}

async fn recv_observed(
    receiver: &mut mpsc::UnboundedReceiver<ObservedRequest>,
) -> Result<ObservedRequest> {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .context("fake upstream observation deadline")?
        .context("fake upstream observation channel closed")
}

async fn ensure_no_upstream(
    receiver: &mut mpsc::UnboundedReceiver<ObservedRequest>,
    case: &str,
) -> Result<()> {
    ensure!(
        tokio::time::timeout(Duration::from_millis(100), receiver.recv())
            .await
            .is_err(),
        "fake upstream received {case}"
    );
    Ok(())
}

async fn fake_upstream(State(state): State<FakeState>, request: Request<Body>) -> Response {
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let hop_sentinel_present = request.headers().contains_key("x-hop-sentinel");
    let body = match axum::body::to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return static_response(StatusCode::BAD_REQUEST, "fake body read failed"),
    };
    let json = serde_json::from_slice::<Value>(&body).ok();
    let stream_requested = json
        .as_ref()
        .and_then(|value| value.get("stream"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let _ = state.observed.send(ObservedRequest {
        path: path.clone(),
        json,
        authorization,
        content_type,
        hop_sentinel_present,
    });

    if path.starts_with("/v1/models") {
        let mut response = static_response(StatusCode::OK, r#"{"data":[]}"#);
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        return response;
    }
    if stream_requested {
        let chunks = stream::unfold(0_u8, |step| async move {
            match step {
                0 => Some((
                    Ok::<_, Infallible>(Bytes::from_static(b"data: {\"step\":1}\n\n")),
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Some((
                        Ok::<_, Infallible>(Bytes::from_static(b"data: [DONE]\n\n")),
                        2,
                    ))
                }
                _ => None,
            }
        });
        let mut response = Response::new(Body::from_stream(chunks));
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        response.headers_mut().insert(
            "x-upstream-witness",
            axum::http::HeaderValue::from_static("sse"),
        );
        return response;
    }

    let mut response = static_response(StatusCode::CREATED, r#"{"ok":true}"#);
    response.headers_mut().insert(
        "content-type",
        axum::http::HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        "x-upstream-witness",
        axum::http::HeaderValue::from_static("json"),
    );
    response
}

fn static_response(status: StatusCode, body: &'static str) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
}
