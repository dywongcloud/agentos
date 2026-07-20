//! Loopback auth-injecting reverse proxy for the Tinfoil inference fallback.
//!
//! ## Why a proxy at all (and not an env var)
//!
//! When the H Company hosted backend rate-limits, the daemon fails over to Tinfoil's
//! OpenAI-compatible endpoint (`https://inference.tinfoil.sh/v1`, model `kimi-k2-6` -- a
//! vision model, per docs.tinfoil.sh/models/vision) by respawning `holo serve` with
//! `--base-url` pointed here. Tinfoil requires `Authorization: Bearer <key>` (witnessed:
//! bare-key `Authorization`, `X-Api-Key`, and `api-key` all 401). The hai-agent-runtime
//! offers exactly two ways to influence that header, and both were witnessed dead ends:
//!
//! - `OPENAI_API_KEY`: ignored by the runtime's vLLM adapter -- its client key comes from
//!   `getenv("HAI_API_KEY")` (Nuitka string dump of `hai_adapters.dispatchers`), which the
//!   launcher deliberately pops whenever a custom base URL is set (`launcher.py::
//!   runtime_child_env`). Witnessed live: 401 `Incorrect API key` with `OPENAI_API_KEY` set.
//! - `HAI_EXTRA_HEADERS`: parsed as SPACE-separated `k=v` pairs, so the value
//!   `Bearer <key>` -- which contains a space -- is structurally inexpressible. Witnessed
//!   live: `httpcore.LocalProtocolError: Illegal header name b'tk_...,X-Holoiroh'` when the
//!   value was smuggled through anyway.
//!
//! So the daemon owns the auth layer instead: `holo serve` talks plain HTTP to
//! `127.0.0.1:<port>/v1/...` (no key anywhere in its env, same shape as the local
//! llama-server path in [`crate::local_model`]) and this proxy forwards each request to the
//! upstream with the real bearer key attached, streaming both bodies (request bodies carry
//! multi-hundred-KB base64 screenshots; responses may be SSE).
//!
//! Bound to `127.0.0.1` only, never a caller-supplied host -- structurally unreachable
//! off-box, matching `local_model.rs`'s defense-in-depth posture for loopback listeners.

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Default upstream. Override via `HOLOIROH_FALLBACK_UPSTREAM` (scheme + host, no trailing
/// slash; the request path -- `/v1/chat/completions` etc. -- is appended verbatim).
pub const DEFAULT_UPSTREAM: &str = "https://inference.tinfoil.sh";

struct ProxyState {
    upstream: String,
    bearer: String,
    client: reqwest::Client,
}

/// The running proxy. Dropping it aborts the serve task (the daemon holds one for its
/// whole lifetime, mirroring how `LocalModelServer` is held).
pub struct TinfoilProxy {
    local_url: String,
    task: JoinHandle<()>,
}

impl TinfoilProxy {
    /// Bind `127.0.0.1:0` (ephemeral port) and start forwarding. `api_key` is the Tinfoil
    /// bearer key (from `TINFOIL_API_KEY` in the gitignored `.env`); it lives only inside
    /// this process -- it is never placed in any child's env or argv.
    pub async fn spawn(upstream: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        let upstream = upstream.into().trim_end_matches('/').to_string();
        let state = Arc::new(ProxyState {
            upstream,
            bearer: format!("Bearer {}", api_key.into()),
            client: reqwest::Client::new(),
        });

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("tinfoil proxy failed to bind a loopback port")?;
        let local_addr = listener.local_addr().context("tinfoil proxy local_addr")?;
        let local_url = format!("http://{local_addr}");

        let app = axum::Router::new()
            .fallback(axum::routing::any(forward))
            .with_state(state);

        let task = tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                tracing::error!(error = %err, "tinfoil proxy server exited");
            }
        });

        tracing::info!(local_url = %local_url, "tinfoil fallback proxy listening (loopback only)");
        Ok(Self { local_url, task })
    }

    /// Base URL `holo serve` should be pointed at (append `/v1` at the call site, matching
    /// the local-model convention where the OpenAI routes live under `/v1`).
    pub fn local_url(&self) -> &str {
        &self.local_url
    }
}

impl Drop for TinfoilProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Forward one request to the upstream with the bearer key injected. Everything is
/// streamed; headers are copied by name/value except the ones this proxy owns
/// (`authorization`, `host`) and the hop-by-hop set the HTTP layers manage themselves.
async fn forward(State(state): State<Arc<ProxyState>>, req: axum::extract::Request) -> Response {
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let url = format!("{}{}", state.upstream, path_and_query);

    let reqwest_method = match reqwest::Method::from_bytes(method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return status_response(StatusCode::METHOD_NOT_ALLOWED, "unsupported method"),
    };
    let mut upstream_req = state.client.request(reqwest_method, &url);

    for (name, value) in req.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        // `authorization` is replaced below (the runtime sends a placeholder key that the
        // upstream must never see); `host`/`content-length` are recomputed by reqwest;
        // hop-by-hop headers must not be forwarded.
        if matches!(
            lower.as_str(),
            "authorization" | "host" | "content-length" | "connection" | "transfer-encoding"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            upstream_req = upstream_req.header(name.as_str(), v);
        }
    }
    upstream_req = upstream_req.header("authorization", &state.bearer);

    // Chat-completion bodies get buffered and REWRITTEN, not streamed: the runtime's requests
    // carry a hardcoded `logit_bias` token id from HOLO's tokenizer (witnessed: 248069), which
    // is out-of-vocab for the fallback model's tokenizer -- Tinfoil's vLLM hard-rejects the
    // whole request with 400 `logit_bias contain out-of-vocab token ids`. The bias only nudges
    // a Holo-specific token, so dropping it is the correct translation for a foreign model.
    // Everything else (guided_json, streaming responses, other routes) passes through untouched.
    let is_chat_completion = req.method() == axum::http::Method::POST
        && req.uri().path().ends_with("/chat/completions");
    if is_chat_completion {
        // 64 MiB cap: screenshots ride as base64 (hundreds of KB each, up to a few per
        // request); anything past this cap is not a legitimate inference request.
        let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024).await {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(error = %err, "tinfoil proxy failed to read request body");
                return status_response(StatusCode::BAD_GATEWAY, "request body read failed");
            }
        };
        let body = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(mut json) => {
                if let Some(obj) = json.as_object_mut() {
                    if obj.remove("logit_bias").is_some() {
                        tracing::debug!("tinfoil proxy stripped logit_bias (holo-tokenizer-specific) from request");
                    }
                }
                serde_json::to_vec(&json).unwrap_or_else(|_| bytes.to_vec())
            }
            // Not JSON? Forward verbatim; the upstream owns rejecting it.
            Err(_) => bytes.to_vec(),
        };
        upstream_req = upstream_req.body(body);
    } else {
        let body_stream = req.into_body().into_data_stream();
        upstream_req = upstream_req.body(reqwest::Body::wrap_stream(body_stream));
    }

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(url = %url, error = %err, "tinfoil proxy upstream request failed");
            return status_response(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
    };

    let status = StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    for (name, value) in upstream_resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "content-length" | "connection" | "transfer-encoding"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            builder = builder.header(name.as_str(), v);
        }
    }
    builder
        .body(Body::from_stream(upstream_resp.bytes_stream()))
        .unwrap_or_else(|_| {
            status_response(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        })
}

fn status_response(status: StatusCode, msg: &'static str) -> Response {
    Response::builder()
        .status(status)
        .body(Body::from(msg))
        .expect("static response build cannot fail")
}
