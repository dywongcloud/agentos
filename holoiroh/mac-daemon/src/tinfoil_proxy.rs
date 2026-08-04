//! This is a loopback, auth-injecting reverse proxy for the Tinfoil inference fallback.
//!
//! ## Why a proxy at all (and not an env var)
//!
//! When the H Company hosted backend rate-limits, the daemon fails over to Tinfoil's
//! OpenAI-compatible endpoint (`https://inference.tinfoil.sh/v1`, model `kimi-k2-6` -- a
//! vision model, per docs.tinfoil.sh/models/vision). It does this by respawning `holo serve`
//! with `--base-url` pointed here. Tinfoil requires `Authorization: Bearer <key>` (witnessed:
//! bare-key `Authorization`, `X-Api-Key`, and `api-key` all return 401). The hai-agent-runtime
//! offers exactly two ways to influence that header. Both are witnessed dead ends:
//!
//! - `OPENAI_API_KEY`: the runtime's vLLM adapter ignores this variable. Its client key comes
//!   from `getenv("HAI_API_KEY")` (Nuitka string dump of `hai_adapters.dispatchers`). The
//!   launcher deliberately pops `HAI_API_KEY` whenever a custom base URL is set
//!   (`launcher.py::runtime_child_env`). Witnessed live: 401 `Incorrect API key`, with
//!   `OPENAI_API_KEY` set.
//! - `HAI_EXTRA_HEADERS`: this variable is parsed as SPACE-separated `k=v` pairs. The value
//!   `Bearer <key>` contains a space, so it is structurally inexpressible here. Witnessed
//!   live: `httpcore.LocalProtocolError: Illegal header name b'tk_...,X-Holoiroh'`, when the
//!   value was smuggled through anyway.
//!
//! So the daemon owns the auth layer instead. `holo serve` talks plain HTTP to
//! `127.0.0.1:<port>/v1/...`, with no key anywhere in its env (the same shape as the local
//! llama-server path in [`crate::local_model`]). This proxy forwards each request to the
//! upstream with the real bearer key attached, streaming both bodies. Request bodies carry
//! multi-hundred-KB base64 screenshots. Responses may be SSE.
//!
//! This proxy binds to `127.0.0.1` only, never a caller-supplied host. It is structurally
//! unreachable off-box, matching `local_model.rs`'s defense-in-depth posture for loopback
//! listeners.

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

struct ProxyState {
    upstream: String,
    client: Arc<crate::tinfoil_client::TinfoilClient>,
}

/// The running proxy. Dropping it aborts the serve task. The daemon holds one proxy for its
/// whole lifetime, mirroring how `LocalModelServer` is held.
pub struct TinfoilProxy {
    local_url: String,
    task: JoinHandle<()>,
}

impl TinfoilProxy {
    /// Binds `127.0.0.1:0`, an ephemeral port, and starts forwarding. `api_key` is the Tinfoil
    /// bearer key, from `TINFOIL_API_KEY` in the gitignored `.env`. It lives only inside this
    /// process. This function never places it in any child's env or argv.
    pub async fn spawn(client: Arc<crate::tinfoil_client::TinfoilClient>) -> Result<Self> {
        let upstream = client.base_url();
        let state = Arc::new(ProxyState { upstream, client });

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

    /// Base URL that `holo serve` should point at. Append `/v1` at the call site. This matches
    /// the local-model convention, where the OpenAI routes live under `/v1`.
    pub fn local_url(&self) -> &str {
        &self.local_url
    }
}

impl Drop for TinfoilProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Forwards one request to the upstream, with the bearer key injected. This function streams
/// everything. It copies headers by name and value, except the ones this proxy owns
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
    let verified_client = match state.client.client().http_client() {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "tinfoil proxy verified client unavailable");
            return status_response(StatusCode::BAD_GATEWAY, "verified upstream unavailable");
        }
    };
    let mut upstream_req = verified_client.request(reqwest_method, &url);

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
    upstream_req = upstream_req.header("authorization", state.client.bearer());

    // Chat-completion bodies get buffered and REWRITTEN, not streamed: the runtime's requests
    // carry a hardcoded `logit_bias` token id from HOLO's tokenizer (witnessed: 248069), which
    // is out-of-vocab for the fallback model's tokenizer -- Tinfoil's vLLM hard-rejects the
    // whole request with 400 `logit_bias contain out-of-vocab token ids`. The bias only nudges
    // a Holo-specific token, so dropping it is the correct translation for a foreign model.
    // Everything else (guided_json, streaming responses, other routes) passes through untouched.
    let is_chat_completion =
        req.method() == axum::http::Method::POST && req.uri().path().ends_with("/chat/completions");
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
                        tracing::debug!(
                            "tinfoil proxy stripped logit_bias (holo-tokenizer-specific) from request"
                        );
                    }
                    let is_kimi = obj.get("model").and_then(|m| m.as_str()) == Some("kimi-k2-6");
                    if is_kimi {
                        apply_kimi_tuning(obj);
                    }
                    if let Err(err) = redact_image_urls_in_messages(obj) {
                        tracing::warn!(error = %err, "tinfoil proxy rejected request because image redaction failed");
                        return status_response(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "image redaction failed; request was not forwarded",
                        );
                    }
                }
                match serde_json::to_vec(&json) {
                    Ok(body) => body,
                    Err(err) => {
                        tracing::warn!(error = %err, "tinfoil proxy failed to serialize redacted request");
                        return status_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "redacted request serialization failed",
                        );
                    }
                }
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

/// Kimi K2-specific request tuning. This function applies the tuning only when `model ==
/// "kimi-k2-6"`. It never touches the primary Holo3-35B request shape. Every change here is a
/// measured fix for a real, reproduced failure on this exact tinfoil deployment, not a
/// speculative tweak. Each fix was probed directly against
/// `https://inference.tinfoil.sh/v1/chat/completions`, with the real `mac-daemon/.env` key, on
/// 2026-07-20:
///
/// 1. **`chat_template_kwargs: {"thinking": false}`**: a real vLLM chat-template parameter.
///    This parameter is not `extra_body`-wrapped. Wrapping it there was measured to have no
///    effect. Kimi K2 is a heavy reasoner. A trivial "say ok" prompt burned ~1100-1300
///    *reasoning* tokens before the model even started its answer (measured completion_tokens,
///    with `thinking` unset). With `thinking: false`, the same prompt dropped to 191 completion
///    tokens and 3s latency, down from ~17s. It still returned real content at
///    `finish_reason: "stop"`.
/// 2. **`guided_json` -> `response_format: {type: "json_schema", ...}`**: the daemon's own
///    desktop-agent runtime requests structured tool-call output via vLLM's `guided_json`
///    parameter. Measured directly: Kimi's vLLM deployment does NOT honor `guided_json`. It
///    returns the JSON wrapped in a markdown code fence (`` ```json\n{...}\n``` ``), instead of
///    raw structured output. This is a real, silent tool-call-parsing hazard: the same "silent
///    empty/malformed completion" shape that `control.rs`'s failover logic already treats as a
///    backend failure. The OpenAI-standard `response_format: json_schema` parameter WAS
///    measured to work correctly on this same endpoint (clean unwrapped JSON, no fence).
///    Translating the request to it is a straightforward, verified fix, not a workaround.
/// 3. **`max_tokens` floor of 6000**: this function only raises the value, never lowers it, so
///    it respects an explicit larger request from the runtime. The runtime's shipped
///    desktop-agent config caps every model at 2048 completion tokens, tuned for Holo3-35B.
///    `mac-daemon`'s own captured runtime logs confirm this literal value reaches Kimi
///    unmodified. Measured directly: a genuinely complex multi-step prompt (open Mail -> Notes
///    -> Calendar -> reply) exhausted 2048 tokens on reasoning alone, with ZERO answer content
///    and `finish_reason: "length"`. This is a real, reproduced silent-truncation failure, not
///    a hypothetical one. Combined with fix 1 (`thinking: false`), 6000 tokens was measured
///    sufficient for this exact prompt to reach `finish_reason: "stop"` with full content.
fn apply_kimi_tuning(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.entry("chat_template_kwargs".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(ctk) = obj
        .get_mut("chat_template_kwargs")
        .and_then(|v| v.as_object_mut())
    {
        ctk.entry("thinking".to_string())
            .or_insert(serde_json::Value::Bool(false));
    }

    if let Some(guided_json) = obj.remove("guided_json") {
        obj.insert(
            "response_format".to_string(),
            serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "computer_use_action",
                    "schema": guided_json,
                    "strict": true
                }
            }),
        );
        tracing::debug!(
            "tinfoil proxy: translated guided_json -> response_format json_schema for kimi-k2-6"
        );
    }

    // The runtime sends `max_completion_tokens` (confirmed in captured logs: the shipped
    // desktop-agent config's literal `max_completion_tokens: 2048` reaches this proxy
    // unmodified) -- NOT the bare `max_tokens` field this same tinfoil endpoint also accepts
    // when called directly (as this module's probes did). Raise whichever field is actually
    // present; if somehow neither is set, add `max_completion_tokens` (the field this
    // deployment's real traffic uses).
    const MIN_MAX_TOKENS: u64 = 6000;
    for field in ["max_completion_tokens", "max_tokens"] {
        if let Some(current) = obj.get(field).and_then(|v| v.as_u64()) {
            if current < MIN_MAX_TOKENS {
                obj.insert(field.to_string(), serde_json::json!(MIN_MAX_TOKENS));
                tracing::debug!(
                    field,
                    previous = current,
                    "tinfoil proxy: raised {field} to {MIN_MAX_TOKENS} floor for kimi-k2-6"
                );
            }
            return;
        }
    }
    obj.insert(
        "max_completion_tokens".to_string(),
        serde_json::json!(MIN_MAX_TOKENS),
    );
}

/// Walks each image URL in chat messages. It redacts local data images before egress.
/// A malformed or unredactable data image rejects the complete request. External URLs are not
/// local image payloads and pass through unchanged.
fn redact_image_urls_in_messages(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    let Some(messages) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return Ok(());
    };
    let mut redacted_images = 0usize;
    for message in messages.iter_mut() {
        let Some(content) = message.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for part in content.iter_mut() {
            let is_image_url = part.get("type").and_then(|t| t.as_str()) == Some("image_url");
            if !is_image_url {
                continue;
            }
            let Some(url) = part
                .get("image_url")
                .and_then(|iu| iu.get("url"))
                .and_then(|u| u.as_str())
                .map(str::to_string)
            else {
                continue;
            };
            if let Some(redacted_url) = redact_data_url(&url)? {
                if let Some(iu) = part.get_mut("image_url") {
                    iu["url"] = serde_json::Value::String(redacted_url);
                    redacted_images += 1;
                }
            }
        }
    }
    if redacted_images > 0 {
        tracing::info!(
            redacted_images,
            "tinfoil proxy: redacted PII in outbound image(s)"
        );
    }
    Ok(())
}

/// Redacts one data image and returns `None` only for a non-data URL.
fn redact_data_url(url: &str) -> anyhow::Result<Option<String>> {
    let prefix = b"data:image/";
    let is_data_image = url
        .as_bytes()
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix));
    if !is_data_image {
        return Ok(None);
    }
    let comma_idx = url
        .find(',')
        .ok_or_else(|| anyhow::anyhow!("data image URL has no payload separator"))?;
    let header = &url[..comma_idx];
    let is_base64 = header
        .rsplit_once(';')
        .is_some_and(|(_, encoding)| encoding.eq_ignore_ascii_case("base64"));
    if !is_base64 {
        anyhow::bail!("data image URL is not base64 encoded");
    }
    let b64_data = &url[comma_idx + 1..];
    let raw_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64_data)
        .map_err(|e| anyhow::anyhow!("base64 decode failed: {e}"))?;
    let image = image::load_from_memory(&raw_bytes)
        .map_err(|e| anyhow::anyhow!("image decode failed: {e}"))?;
    let (redacted, _count) = crate::privacy::ocr_and_redact(&image)?;

    let mut png_bytes = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        redacted
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| anyhow::anyhow!("PNG re-encode failed: {e}"))?;
    }
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png_bytes);
    Ok(Some(format!("data:image/png;base64,{encoded}")))
}

fn status_response(status: StatusCode, msg: &'static str) -> Response {
    Response::builder()
        .status(status)
        .body(Body::from(msg))
        .expect("static response build cannot fail")
}
