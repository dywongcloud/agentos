//! In-process witness for every direct Tinfoil response-body class.
//!
//! Run: `cargo run --example tinfoil_error_handling_probe -p holoiroh-daemon`

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Response, StatusCode, header};
use axum::routing::get;
use futures_util::stream;
use holoiroh_daemon::clarify::{
    ClarifyConfig, generate_clarifying_questions, parse_clarify_response,
};
use holoiroh_daemon::tinfoil_audio::{
    SPEECH_BASE64_LIMIT_BYTES, encode_speech_base64, parse_transcription_response, speech,
    transcribe,
};
use holoiroh_daemon::tinfoil_client::{
    DOCUMENT_SUCCESS_BODY_LIMIT_BYTES, HTTP_ERROR_BODY_LIMIT_BYTES, JSON_SUCCESS_BODY_LIMIT_BYTES,
    ResponseBodyLimitError, SPEECH_SUCCESS_BODY_LIMIT_BYTES, TinfoilClient,
    collect_bounded_response_body, collect_tinfoil_response,
};
use holoiroh_daemon::tinfoil_documents::{
    ConvertMode, DocumentInput, convert_documents, parse_convert_response,
};
use holoiroh_daemon::tinfoil_planner::{parse_plan_response, plan_task};
use holoiroh_daemon::tinfoil_vision::{VisionModel, analyze_image, parse_image_analysis_response};

const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const SLOW_STREAM_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Default)]
struct TransferMetrics {
    yielded_bytes: AtomicUsize,
    completed: AtomicBool,
}

#[derive(Clone, Default)]
struct FakeState {
    transfers: Arc<Mutex<HashMap<u64, Arc<TransferMetrics>>>>,
}

impl FakeState {
    fn metrics(&self, id: u64) -> Result<Arc<TransferMetrics>> {
        self.transfers
            .lock()
            .map_err(|_| anyhow::anyhow!("fake-server metrics lock was poisoned"))?
            .get(&id)
            .cloned()
            .context("fake-server transfer metrics were unavailable")
    }
}

fn generated_body(
    total_bytes: usize,
    chunk_bytes: usize,
    delay: Duration,
    metrics: Arc<TransferMetrics>,
) -> Body {
    let stream = stream::unfold(total_bytes, move |remaining| {
        let metrics = metrics.clone();
        async move {
            if remaining == 0 {
                metrics.completed.store(true, Ordering::SeqCst);
                return None;
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let length = remaining.min(chunk_bytes);
            metrics.yielded_bytes.fetch_add(length, Ordering::SeqCst);
            let chunk = vec![b'x'; length];
            Some((Ok::<_, Infallible>(chunk), remaining - length))
        }
    });
    Body::from_stream(stream)
}

async fn declared_oversized(Path(size): Path<usize>) -> Response<Body> {
    declared_response(size, StatusCode::OK)
}

async fn declared_error(Path(size): Path<usize>) -> Response<Body> {
    declared_response(size, StatusCode::BAD_GATEWAY)
}

fn declared_response(size: usize, status: StatusCode) -> Response<Body> {
    let body = Body::from_stream(stream::pending::<std::result::Result<Vec<u8>, Infallible>>());
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, size.to_string())
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn generated(
    Path((id, size)): Path<(u64, usize)>,
    State(state): State<FakeState>,
) -> Response<Body> {
    generated_response(
        id,
        size,
        STREAM_CHUNK_BYTES,
        Duration::ZERO,
        StatusCode::OK,
        state,
    )
}

async fn generated_error(
    Path((id, size)): Path<(u64, usize)>,
    State(state): State<FakeState>,
) -> Response<Body> {
    generated_response(
        id,
        size,
        STREAM_CHUNK_BYTES,
        Duration::ZERO,
        StatusCode::BAD_GATEWAY,
        state,
    )
}

async fn generated_slow(
    Path((id, size)): Path<(u64, usize)>,
    State(state): State<FakeState>,
) -> Response<Body> {
    generated_response(
        id,
        size,
        SLOW_STREAM_CHUNK_BYTES,
        Duration::from_millis(1),
        StatusCode::OK,
        state,
    )
}

fn generated_response(
    id: u64,
    size: usize,
    chunk_bytes: usize,
    delay: Duration,
    status: StatusCode,
    state: FakeState,
) -> Response<Body> {
    let metrics = Arc::new(TransferMetrics::default());
    if let Ok(mut transfers) = state.transfers.lock() {
        transfers.insert(id, metrics.clone());
    }
    let mut response = Response::new(generated_body(size, chunk_bytes, delay, metrics));
    *response.status_mut() = status;
    response
}

fn fixture(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(body.into())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn document_fixture() -> Response<Body> {
    fixture(
        StatusCode::OK,
        r#"{"document":{"md_content":"hello world"},"status":"success"}"#,
    )
}

async fn transcription_fixture() -> Response<Body> {
    fixture(StatusCode::OK, r#"{"text":"fixture transcript"}"#)
}

async fn speech_fixture() -> Response<Body> {
    fixture(StatusCode::OK, b"RIFFfixtureWAVE".as_slice())
}

async fn vision_fixture() -> Response<Body> {
    fixture(
        StatusCode::OK,
        r#"{"choices":[{"message":{"content":"fixture image answer"}}]}"#,
    )
}

async fn planner_fixture() -> Response<Body> {
    fixture(
        StatusCode::OK,
        r#"{"choices":[{"message":{"content":"fixture plan","tool_calls":[]}}]}"#,
    )
}

async fn clarify_fixture() -> Response<Body> {
    fixture(
        StatusCode::OK,
        serde_json::json!({
            "choices": [{
                "message": {
                    "content": "{\"questions\":[{\"question\":\"Which item?\",\"options\":[\"First\",\"Second\"]}]}"
                }
            }]
        })
        .to_string(),
    )
}

async fn text_error_fixture() -> Response<Body> {
    fixture(
        StatusCode::BAD_GATEWAY,
        " upstream\nfailed\twithout secrets \r ",
    )
}

async fn binary_error_fixture() -> Response<Body> {
    fixture(StatusCode::SERVICE_UNAVAILABLE, vec![0, 159, 146, 150, 255])
}

struct BodyClass {
    name: &'static str,
    limit: usize,
}

async fn fetch_success(
    client: &reqwest::Client,
    url: String,
    limit: usize,
    operation: &str,
) -> Result<Vec<u8>> {
    let response = client.get(url).send().await?;
    collect_tinfoil_response(response, limit, operation).await
}

fn load_live_key() -> Option<String> {
    let env = std::fs::read_to_string("mac-daemon/.env").ok()?;
    env.lines().find_map(|line| {
        line.trim()
            .strip_prefix("TINFOIL_API_KEY=")
            .map(|value| {
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string()
            })
            .filter(|value| !value.is_empty())
    })
}

async fn run_live_regression(key: String) -> Result<()> {
    let client = Arc::new(
        TinfoilClient::new(key)
            .await
            .map_err(|_| anyhow::anyhow!("live Tinfoil attestation failed"))?,
    );

    let documents = convert_documents(
        &client,
        &[DocumentInput {
            filename: "response-limit-probe.csv".to_string(),
            bytes: b"name,role\nAda,Engineer\n".to_vec(),
        }],
        ConvertMode::Text,
    )
    .await
    .map_err(|_| anyhow::anyhow!("live document conversion failed"))?;
    let document_bytes: usize = documents
        .iter()
        .map(|document| document.markdown.len())
        .sum();
    if documents.is_empty() || document_bytes == 0 {
        bail!("live document conversion returned no content");
    }

    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        32,
        32,
        image::Rgba([220, 20, 20, 255]),
    ));
    let vision = analyze_image(
        &client,
        &image,
        "Identify the solid color in one word.",
        VisionModel::Gemma431b,
    )
    .await
    .map_err(|_| anyhow::anyhow!("live image analysis failed"))?;

    let wav = speech(&client, "Testing one two three.", "serena")
        .await
        .map_err(|_| anyhow::anyhow!("live speech synthesis failed"))?;
    let wav_bytes = wav.len();
    let transcript = transcribe(&client, wav, "response-limit-probe.wav")
        .await
        .map_err(|_| anyhow::anyhow!("live transcription failed"))?;

    let plan = plan_task(&client, "Open Safari and check the weather")
        .await
        .map_err(|_| anyhow::anyhow!("live planning failed"))?;
    let plan_bytes = plan.plan_id.len() + plan.goal_digest.len();

    let clarify = generate_clarifying_questions(
        "Send the file to the team",
        &ClarifyConfig::new(client.clone()),
    )
    .await;
    if clarify.is_empty() {
        bail!("live clarification returned no questions");
    }
    let clarify_bytes: usize = clarify
        .iter()
        .map(|question| {
            question.question.len() + question.options.iter().map(String::len).sum::<usize>()
        })
        .sum();

    println!(
        "live regression -> documents={} document_bytes={} vision_bytes={} WAV_bytes={} transcript_bytes={} plan_steps={} plan_bytes={} clarify_questions={} clarify_bytes={}",
        documents.len(),
        document_bytes,
        vision.len(),
        wav_bytes,
        transcript.len(),
        plan.steps.len(),
        plan_bytes,
        clarify.len(),
        clarify_bytes
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = FakeState::default();
    let app = Router::new()
        .route("/declared/{size}", get(declared_oversized))
        .route("/declared-error/{size}", get(declared_error))
        .route("/generated/{id}/{size}", get(generated))
        .route("/generated-error/{id}/{size}", get(generated_error))
        .route("/slow/{id}/{size}", get(generated_slow))
        .route("/fixture/document", get(document_fixture))
        .route("/fixture/transcription", get(transcription_fixture))
        .route("/fixture/speech", get(speech_fixture))
        .route("/fixture/vision", get(vision_fixture))
        .route("/fixture/planner", get(planner_fixture))
        .route("/fixture/clarify", get(clarify_fixture))
        .route("/fixture/error-text", get(text_error_fixture))
        .route("/fixture/error-binary", get(binary_error_fixture))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            eprintln!("fake Tinfoil server stopped: {error}");
        }
    });
    let base = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    let classes = [
        BodyClass {
            name: "document",
            limit: DOCUMENT_SUCCESS_BODY_LIMIT_BYTES,
        },
        BodyClass {
            name: "speech-wav",
            limit: SPEECH_SUCCESS_BODY_LIMIT_BYTES,
        },
        BodyClass {
            name: "vision-json",
            limit: JSON_SUCCESS_BODY_LIMIT_BYTES,
        },
        BodyClass {
            name: "planner-json",
            limit: JSON_SUCCESS_BODY_LIMIT_BYTES,
        },
        BodyClass {
            name: "clarify-json",
            limit: JSON_SUCCESS_BODY_LIMIT_BYTES,
        },
        BodyClass {
            name: "transcription-json",
            limit: JSON_SUCCESS_BODY_LIMIT_BYTES,
        },
        BodyClass {
            name: "http-error",
            limit: HTTP_ERROR_BODY_LIMIT_BYTES,
        },
    ];

    let mut transfer_id = 1u64;
    for class in classes {
        let oversized = class.limit + 1;
        let response = client
            .get(format!("{base}/declared/{oversized}"))
            .send()
            .await?;
        match collect_bounded_response_body(response, class.limit).await {
            Err(ResponseBodyLimitError::DeclaredTooLarge { declared, limit }) => {
                assert_eq!(declared, oversized as u64);
                assert_eq!(limit, class.limit);
                println!(
                    "{} Content-Length {} -> rejected before read; retained=0",
                    class.name, oversized
                );
            }
            other => bail!(
                "{} declared oversized boundary produced unexpected result: {other:?}",
                class.name
            ),
        }

        let id = transfer_id;
        transfer_id += 1;
        let response = client
            .get(format!("{base}/generated/{id}/{oversized}"))
            .send()
            .await?;
        match collect_bounded_response_body(response, class.limit).await {
            Err(ResponseBodyLimitError::StreamTooLarge {
                limit,
                retained,
                observed_at_least,
            }) => {
                assert_eq!(limit, class.limit);
                assert!(retained <= class.limit);
                assert!(observed_at_least > class.limit);
                println!(
                    "{} chunked {} -> rejected; retained={} observed_at_least={}",
                    class.name, oversized, retained, observed_at_least
                );
            }
            other => bail!(
                "{} chunked cap+1 boundary produced unexpected result: {other:?}",
                class.name
            ),
        }

        let id = transfer_id;
        transfer_id += 1;
        let response = client
            .get(format!("{base}/generated/{id}/{}", class.limit))
            .send()
            .await?;
        let accepted = collect_bounded_response_body(response, class.limit).await?;
        assert_eq!(accepted.len(), class.limit);
        println!(
            "{} exact cap {} -> accepted len={}",
            class.name,
            class.limit,
            accepted.len()
        );
        drop(accepted);
    }

    let oversized_error = HTTP_ERROR_BODY_LIMIT_BYTES + 1;
    let declared_error = collect_tinfoil_response(
        client
            .get(format!("{base}/declared-error/{oversized_error}"))
            .send()
            .await?,
        DOCUMENT_SUCCESS_BODY_LIMIT_BYTES,
        "declared error fixture",
    )
    .await
    .expect_err("oversized declared HTTP error must fail before reading")
    .to_string();
    assert!(declared_error.contains("502 Bad Gateway"));
    assert!(declared_error.contains("65536"));

    let id = transfer_id;
    transfer_id += 1;
    let chunked_error = collect_tinfoil_response(
        client
            .get(format!("{base}/generated-error/{id}/{oversized_error}"))
            .send()
            .await?,
        DOCUMENT_SUCCESS_BODY_LIMIT_BYTES,
        "chunked error fixture",
    )
    .await
    .expect_err("oversized chunked HTTP error must fail at cap+1")
    .to_string();
    assert!(chunked_error.contains("502 Bad Gateway"));
    assert!(chunked_error.contains("65536"));

    let id = transfer_id;
    transfer_id += 1;
    let exact_error = collect_tinfoil_response(
        client
            .get(format!(
                "{base}/generated-error/{id}/{HTTP_ERROR_BODY_LIMIT_BYTES}"
            ))
            .send()
            .await?,
        DOCUMENT_SUCCESS_BODY_LIMIT_BYTES,
        "exact error fixture",
    )
    .await
    .expect_err("exact-cap HTTP error remains an HTTP error")
    .to_string();
    assert!(exact_error.contains("502 Bad Gateway"));
    assert!(exact_error.len() < 1200);
    println!("HTTP error status path -> 65537 rejected and 65536 sanitized without large output");

    let slow_total = HTTP_ERROR_BODY_LIMIT_BYTES + 16 * 1024 * 1024;
    let slow_id = transfer_id;
    let response = client
        .get(format!("{base}/slow/{slow_id}/{slow_total}"))
        .send()
        .await?;
    match collect_bounded_response_body(response, HTTP_ERROR_BODY_LIMIT_BYTES).await {
        Err(ResponseBodyLimitError::StreamTooLarge { retained, .. }) => {
            assert!(retained <= HTTP_ERROR_BODY_LIMIT_BYTES);
        }
        other => bail!("slow oversized response produced unexpected result: {other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
    let metrics = state.metrics(slow_id)?;
    let yielded = metrics.yielded_bytes.load(Ordering::SeqCst);
    let completed = metrics.completed.load(Ordering::SeqCst);
    assert!(yielded < slow_total);
    assert!(!completed);
    println!(
        "oversized slow stream -> client stopped server early; yielded={yielded} total={slow_total} completed={completed}"
    );

    let document = fetch_success(
        &client,
        format!("{base}/fixture/document"),
        DOCUMENT_SUCCESS_BODY_LIMIT_BYTES,
        "document fixture",
    )
    .await?;
    let document = parse_convert_response(std::str::from_utf8(&document)?)?;
    assert_eq!(document[0].markdown, "hello world");

    let transcription = fetch_success(
        &client,
        format!("{base}/fixture/transcription"),
        JSON_SUCCESS_BODY_LIMIT_BYTES,
        "transcription fixture",
    )
    .await?;
    assert_eq!(
        parse_transcription_response(&transcription)?,
        "fixture transcript"
    );

    let speech = fetch_success(
        &client,
        format!("{base}/fixture/speech"),
        SPEECH_SUCCESS_BODY_LIMIT_BYTES,
        "speech fixture",
    )
    .await?;
    assert!(speech.starts_with(b"RIFF") && speech.ends_with(b"WAVE"));

    let vision = fetch_success(
        &client,
        format!("{base}/fixture/vision"),
        JSON_SUCCESS_BODY_LIMIT_BYTES,
        "vision fixture",
    )
    .await?;
    assert_eq!(
        parse_image_analysis_response(&vision)?,
        "fixture image answer"
    );

    let planner = fetch_success(
        &client,
        format!("{base}/fixture/planner"),
        JSON_SUCCESS_BODY_LIMIT_BYTES,
        "planner fixture",
    )
    .await?;
    let fixture_goal = holoiroh_daemon::tinfoil_planner::TrustedGoal::new("fixture goal")?;
    assert!(
        parse_plan_response(&planner, &fixture_goal).is_err(),
        "free-text planner fallback must be rejected"
    );

    let clarify = fetch_success(
        &client,
        format!("{base}/fixture/clarify"),
        JSON_SUCCESS_BODY_LIMIT_BYTES,
        "clarify fixture",
    )
    .await?;
    let questions = parse_clarify_response(&clarify)?;
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].question, "Which item?");
    println!("all normal fixtures -> bounded collection and parse OK");

    let error = collect_tinfoil_response(
        client
            .get(format!("{base}/fixture/error-text"))
            .send()
            .await?,
        JSON_SUCCESS_BODY_LIMIT_BYTES,
        "error fixture",
    )
    .await
    .expect_err("HTTP error fixture must remain an error")
    .to_string();
    assert!(error.contains("502 Bad Gateway"));
    assert!(error.contains("upstream failed without secrets"));
    assert!(!error.contains('\n') && !error.contains('\r') && !error.contains('\t'));

    let binary_error = collect_tinfoil_response(
        client
            .get(format!("{base}/fixture/error-binary"))
            .send()
            .await?,
        JSON_SUCCESS_BODY_LIMIT_BYTES,
        "binary error fixture",
    )
    .await
    .expect_err("binary HTTP error fixture must remain an error")
    .to_string();
    assert!(binary_error.contains("503 Service Unavailable"));
    assert!(binary_error.contains("non-text response body (5 bytes)"));
    println!("HTTP errors -> status plus bounded sanitized text OK");

    let exact_wav = vec![0u8; SPEECH_SUCCESS_BODY_LIMIT_BYTES];
    let exact_base64 = encode_speech_base64(&exact_wav);
    assert_eq!(exact_base64.len(), SPEECH_BASE64_LIMIT_BYTES);
    println!(
        "TTS base64 exact WAV {} -> accepted encoded_len={}",
        exact_wav.len(),
        exact_base64.len()
    );
    drop(exact_base64);
    drop(exact_wav);

    let oversized_wav = vec![0u8; SPEECH_SUCCESS_BODY_LIMIT_BYTES + 1];
    let oversized_base64 = encode_speech_base64(&oversized_wav);
    assert!(oversized_base64.is_empty());
    println!(
        "TTS base64 WAV {} -> rejected before encoded allocation",
        oversized_wav.len()
    );
    drop(oversized_wav);

    server.abort();
    if std::env::var("HOLOIROH_RUN_TINFOIL_LIVE").as_deref() == Ok("1") {
        let key = load_live_key().context("real ignored TINFOIL_API_KEY was unavailable")?;
        run_live_regression(key).await?;
    }
    println!("tinfoil_error_handling_probe: OK");
    Ok(())
}
