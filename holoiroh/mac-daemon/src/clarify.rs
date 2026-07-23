//! Clarifying-questions inference: given a possibly-ambiguous user instruction,
//! ask a Tinfoil-hosted model to produce 1-3 clarifying questions (each with a
//! few concrete options) BEFORE the desktop agent runs. Empty questions means
//! the instruction was already clear enough to run as-is.
//!
//! Entirely best-effort and OFF the desktop-task path: any failure (no key,
//! timeout, malformed model output) returns an empty list, so a prompt is never
//! blocked or lost by the clarification layer. The Tinfoil bearer key lives only
//! inside this daemon process -- same posture as [`crate::tinfoil_proxy`] -- and
//! is never placed in any child's env or argv.

use std::time::Duration;

use anyhow::Result;
use holoiroh_wire::ClarifyingQuestion;
use serde::Deserialize;

const TINFOIL_ENDPOINT: &str = "https://inference.tinfoil.sh/v1/chat/completions";

/// Default clarification model. Tinfoil's catalog has no DeepSeek model as of
/// this writing (witnessed model ids: kimi-k2-6, glm-5-2, gemma4-31b,
/// llama3-3-70b, gpt-oss-120b, ...), so the originally-requested "DeepSeek v4
/// pro" is expressed as a swappable default here -- point `HOLOIROH_CLARIFY_MODEL`
/// at a DeepSeek id the moment Tinfoil adds one. `gpt-oss-120b` is the strongest
/// general reasoner available and was verified to emit clean structured
/// clarifying questions (and an empty list for clear prompts) under
/// `response_format: json_object`.
const DEFAULT_CLARIFY_MODEL: &str = "gpt-oss-120b";

const CLARIFY_SYSTEM_PROMPT: &str = "You are a clarification assistant for a computer-use agent that controls the Mac. If the user instruction is ambiguous or underspecified enough that acting could do the wrong thing, produce 1 to 3 clarifying questions, each with 2 or 3 concrete answer options. If the instruction is already clear and specific, return an empty questions list. Respond ONLY as a JSON object with a single key named questions, whose value is an array of objects, each having a question string field and an options array-of-strings field.";

/// The clarification backend config: the Tinfoil bearer key plus the model id to
/// request. Cheaply cloned into every accepted control connection.
#[derive(Clone)]
pub struct ClarifyConfig {
    api_key: String,
    model: String,
}

impl ClarifyConfig {
    /// Builds a config from the Tinfoil key, resolving the model from
    /// `HOLOIROH_CLARIFY_MODEL` (falling back to [`DEFAULT_CLARIFY_MODEL`]).
    pub fn new(api_key: String) -> Self {
        let model = std::env::var("HOLOIROH_CLARIFY_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_CLARIFY_MODEL.to_string());
        Self { api_key, model }
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Generates clarifying questions for `prompt`. Never returns an error: any
/// failure logs and yields an empty list so the caller proceeds with a direct
/// send.
pub async fn generate_clarifying_questions(
    prompt: &str,
    config: &ClarifyConfig,
) -> Vec<ClarifyingQuestion> {
    match try_generate(prompt, config).await {
        Ok(questions) => questions,
        Err(err) => {
            tracing::warn!(error = %err, "clarify inference failed; proceeding without questions");
            Vec::new()
        }
    }
}

async fn try_generate(prompt: &str, config: &ClarifyConfig) -> Result<Vec<ClarifyingQuestion>> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let capped: String = trimmed.chars().take(4000).collect();

    let body = serde_json::json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": CLARIFY_SYSTEM_PROMPT},
            {"role": "user", "content": capped},
        ],
        "response_format": {"type": "json_object"},
        "max_tokens": 700,
        "temperature": 0.2,
    });

    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(20),
        client
            .post(TINFOIL_ENDPOINT)
            .header("authorization", format!("Bearer {}", config.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send(),
    )
    .await??;

    if !response.status().is_success() {
        anyhow::bail!("clarify upstream returned {}", response.status());
    }

    let value: serde_json::Value = response.json().await?;
    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default();

    Ok(parse_questions(content))
}

#[derive(Deserialize)]
struct QuestionsEnvelope {
    #[serde(default)]
    questions: Vec<ClarifyingQuestion>,
}

/// Parses the model's JSON content into at most three questions with at most
/// three concrete options each (the app appends its own "Something else…"
/// option). Tolerates a stray markdown code fence and drops empty-text
/// questions. Returns empty on any parse failure.
fn parse_questions(content: &str) -> Vec<ClarifyingQuestion> {
    let cleaned = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let envelope: QuestionsEnvelope = match serde_json::from_str(cleaned) {
        Ok(env) => env,
        Err(err) => {
            tracing::warn!(error = %err, "clarify model returned unparseable JSON; no questions");
            return Vec::new();
        }
    };

    envelope
        .questions
        .into_iter()
        .filter(|q| !q.question.trim().is_empty())
        .map(|mut q| {
            q.options.truncate(3);
            q
        })
        .take(3)
        .collect()
}
