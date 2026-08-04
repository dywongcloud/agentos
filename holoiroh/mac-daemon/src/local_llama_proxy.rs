use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::local_model::{LocalModelConfig, MAX_LOCAL_MAX_TOKENS, MIN_LOCAL_MAX_TOKENS};

#[path = "window_crop.rs"]
pub mod window_crop;

use window_crop::{
    CROPPED_RESPONSE_TIMEOUT, CropTransform, MAX_CROPPED_RESPONSE_BYTES,
    SystemWindowSnapshotSource, WindowSnapshotSource,
};

pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_BODY_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const RESPONSE_BODY_TIMEOUT: Duration = Duration::from_secs(600);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

struct ProxyState {
    upstream: SocketAddr,
    max_tokens: u32,
    client: reqwest::Client,
    crop_source: Option<Arc<dyn WindowSnapshotSource>>,
}

pub struct LocalLlamaProxy {
    local_url: String,
    shutdown: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl LocalLlamaProxy {
    pub async fn spawn_for_config(config: &LocalModelConfig) -> Result<Self> {
        let upstream = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), config.port);
        Self::spawn_for_loopback_upstream(upstream, config.max_tokens).await
    }

    pub async fn spawn_for_loopback_upstream(
        upstream: SocketAddr,
        max_tokens: u32,
    ) -> Result<Self> {
        let crop_source = window_crop::crop_enabled_from_env()
            .then(|| Arc::new(SystemWindowSnapshotSource) as Arc<dyn WindowSnapshotSource>);
        Self::spawn_with_crop_source(upstream, max_tokens, crop_source).await
    }

    pub async fn spawn_for_loopback_upstream_with_crop_source(
        upstream: SocketAddr,
        max_tokens: u32,
        crop_source: Arc<dyn WindowSnapshotSource>,
    ) -> Result<Self> {
        Self::spawn_with_crop_source(upstream, max_tokens, Some(crop_source)).await
    }

    async fn spawn_with_crop_source(
        upstream: SocketAddr,
        max_tokens: u32,
        crop_source: Option<Arc<dyn WindowSnapshotSource>>,
    ) -> Result<Self> {
        if !upstream.ip().is_loopback() {
            bail!("local llama proxy upstream must be loopback");
        }
        if !(MIN_LOCAL_MAX_TOKENS..=MAX_LOCAL_MAX_TOKENS).contains(&max_tokens) {
            bail!(
                "local llama proxy max tokens must be in {MIN_LOCAL_MAX_TOKENS}..={MAX_LOCAL_MAX_TOKENS}"
            );
        }

        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(STREAM_IDLE_TIMEOUT)
            .timeout(RESPONSE_BODY_TIMEOUT)
            .build()
            .context("failed to build local llama proxy HTTP client")?;
        let crop_enabled = crop_source.is_some();
        let state = Arc::new(ProxyState {
            upstream,
            max_tokens,
            client,
            crop_source,
        });
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .context("local llama proxy failed to bind loopback")?;
        let local_addr = listener
            .local_addr()
            .context("local llama proxy failed to read its listener address")?;
        let local_url = format!("http://{local_addr}");
        let shutdown = CancellationToken::new();
        let serve_shutdown = shutdown.clone();
        let app = axum::Router::new()
            .fallback(axum::routing::any(forward))
            .with_state(state);
        let task = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app)
                .with_graceful_shutdown(serve_shutdown.cancelled_owned())
                .await
            {
                tracing::error!(error = %err, "local llama proxy server exited");
            }
        });

        tracing::info!(
            local_addr = %local_addr,
            upstream_addr = %upstream,
            max_tokens,
            window_crop_enabled = crop_enabled,
            "local llama proxy listening"
        );
        Ok(Self {
            local_url,
            shutdown,
            task: Some(task),
        })
    }

    pub fn local_url(&self) -> &str {
        &self.local_url
    }

    pub fn base_url(&self) -> String {
        format!("{}/v1", self.local_url())
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.shutdown.cancel();
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(anyhow::Error::new(err).context("local llama proxy task failed")),
            Err(_) => {
                task.abort();
                let _ = task.await;
                bail!("local llama proxy did not stop within {SHUTDOWN_TIMEOUT:?}")
            }
        }
    }
}

impl Drop for LocalLlamaProxy {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn forward(State(state): State<Arc<ProxyState>>, request: Request<Body>) -> Response {
    if request.uri().scheme().is_some() || request.uri().authority().is_some() {
        return error_response(StatusCode::BAD_REQUEST, "invalid request target");
    }
    let Some(path_and_query) = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_owned())
    else {
        return error_response(StatusCode::BAD_REQUEST, "invalid request target");
    };
    if !path_and_query.starts_with('/') {
        return error_response(StatusCode::BAD_REQUEST, "invalid request target");
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let request_headers = filtered_headers(request.headers(), true);
    let request_body = match tokio::time::timeout(
        REQUEST_BODY_TIMEOUT,
        axum::body::to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => {
            tracing::warn!(path = %path, "local llama proxy rejected an oversized request body");
            return error_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
        }
        Err(_) => {
            tracing::warn!(path = %path, "local llama proxy request-body deadline expired");
            return error_response(StatusCode::REQUEST_TIMEOUT, "request body timed out");
        }
    };

    let (forwarded_body, crop_transform) = if path == CHAT_COMPLETIONS_PATH {
        match rewrite_chat_body(
            &request_body,
            state.max_tokens,
            state.crop_source.as_deref(),
        ) {
            Ok(body) => (body.bytes, body.crop_transform),
            Err(()) => {
                tracing::warn!(path = %path, "local llama proxy rejected malformed chat JSON");
                return error_response(StatusCode::BAD_REQUEST, "malformed chat completion JSON");
            }
        }
    } else {
        (request_body.to_vec(), None)
    };

    let upstream_url = format!("http://{}{}", state.upstream, path_and_query);
    let upstream_request = state
        .client
        .request(method, upstream_url)
        .headers(request_headers)
        .body(forwarded_body);
    let upstream_response =
        match tokio::time::timeout(RESPONSE_HEADER_TIMEOUT, upstream_request.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => {
                tracing::warn!(
                    timeout = err.is_timeout(),
                    connect = err.is_connect(),
                    "local llama proxy upstream request failed"
                );
                return error_response(StatusCode::BAD_GATEWAY, "upstream request failed");
            }
            Err(_) => {
                tracing::warn!("local llama proxy upstream response-header deadline expired");
                return error_response(StatusCode::GATEWAY_TIMEOUT, "upstream response timed out");
            }
        };

    let status = StatusCode::from_u16(upstream_response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response_headers = filtered_headers(upstream_response.headers(), false);
    if let Some(transform) = crop_transform
        && status.is_success()
    {
        let content_type = response_headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let response_body = match collect_cropped_response(upstream_response).await {
            Ok(body) => body,
            Err(CroppedResponseFailure::Timeout) => {
                tracing::warn!("local llama proxy cropped response deadline expired");
                return error_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "cropped upstream response timed out",
                );
            }
            Err(CroppedResponseFailure::Oversized) => {
                tracing::warn!(
                    response_cap_bytes = MAX_CROPPED_RESPONSE_BYTES,
                    "local llama proxy rejected oversized cropped response"
                );
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    "cropped upstream response too large",
                );
            }
            Err(CroppedResponseFailure::Read) => {
                tracing::warn!("local llama proxy failed to read cropped response");
                return error_response(StatusCode::BAD_GATEWAY, "cropped upstream response failed");
            }
        };
        let rebased = match window_crop::rebase_response(
            &response_body,
            content_type.as_deref(),
            transform,
        ) {
            Ok(rebased) => rebased,
            Err(err) => {
                tracing::warn!(
                    reason = %err,
                    "local llama proxy rejected unsafe cropped response"
                );
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    "cropped upstream response rejected",
                );
            }
        };
        response_headers.remove(CONTENT_LENGTH);
        if let Ok(value) = HeaderValue::from_str(&rebased.bytes.len().to_string()) {
            response_headers.insert(CONTENT_LENGTH, value);
        }
        tracing::debug!(
            coordinate_count = rebased.coordinate_count,
            response_bytes = rebased.bytes.len(),
            "local llama proxy rebased cropped response"
        );
        let mut response = Response::new(Body::from(rebased.bytes));
        *response.status_mut() = status;
        *response.headers_mut() = response_headers;
        return response;
    }

    let mut response = Response::new(Body::from_stream(upstream_response.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

struct RewrittenChatBody {
    bytes: Vec<u8>,
    crop_transform: Option<CropTransform>,
}

fn rewrite_chat_body(
    bytes: &[u8],
    max_tokens: u32,
    crop_source: Option<&dyn WindowSnapshotSource>,
) -> Result<RewrittenChatBody, ()> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let object = value.as_object_mut().ok_or(())?;
    object.insert("n_predict".to_string(), serde_json::json!(max_tokens));
    object.insert("cache_prompt".to_string(), serde_json::Value::Bool(true));
    let crop_transform = crop_source.and_then(|source| {
        let outcome = window_crop::crop_chat_request(&mut value, source);
        if let Some(metadata) = outcome.metadata {
            tracing::debug!(
                full_width = metadata.full_width,
                full_height = metadata.full_height,
                crop_width = metadata.crop_width,
                crop_height = metadata.crop_height,
                original_jpeg_bytes = metadata.original_jpeg_bytes,
                cropped_jpeg_bytes = metadata.cropped_jpeg_bytes,
                resolver_micros = metadata.resolver_latency.as_micros(),
                decode_micros = metadata.decode_latency.as_micros(),
                encode_micros = metadata.encode_latency.as_micros(),
                "local llama proxy cropped target window"
            );
        } else if let Some(reason) = outcome.skip_reason
            && reason != window_crop::CropSkipReason::NoImage
        {
            tracing::debug!(
                reason = reason.code(),
                "local llama proxy kept full screenshot"
            );
        }
        outcome.transform
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| ())?;
    Ok(RewrittenChatBody {
        bytes,
        crop_transform,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CroppedResponseFailure {
    Timeout,
    Oversized,
    Read,
}

async fn collect_cropped_response(
    response: reqwest::Response,
) -> Result<Vec<u8>, CroppedResponseFailure> {
    tokio::time::timeout(CROPPED_RESPONSE_TIMEOUT, async move {
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| CroppedResponseFailure::Read)?;
            if body.len().saturating_add(chunk.len()) > MAX_CROPPED_RESPONSE_BYTES {
                return Err(CroppedResponseFailure::Oversized);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })
    .await
    .map_err(|_| CroppedResponseFailure::Timeout)?
}

fn filtered_headers(headers: &HeaderMap, request_direction: bool) -> HeaderMap {
    let connection_names = connection_header_names(headers);
    let mut filtered = HeaderMap::new();
    for (name, value) in headers {
        if is_hop_by_hop(name, &connection_names)
            || (request_direction && (name == HOST || name == CONTENT_LENGTH))
        {
            continue;
        }
        filtered.append(name.clone(), value.clone());
    }
    filtered
}

fn connection_header_names(headers: &HeaderMap) -> HashSet<String> {
    headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn is_hop_by_hop(name: &HeaderName, connection_names: &HashSet<String>) -> bool {
    let lower = name.as_str();
    connection_names.contains(lower)
        || matches!(
            lower,
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "proxy-connection"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        )
}

fn error_response(status: StatusCode, message: &'static str) -> Response {
    let body = serde_json::json!({ "error": message }).to_string();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    response
}
