//! Agentic task planning via Tinfoil's tool-calling (docs.tinfoil.sh/guides/tool-calling:
//! "GLM-5.2 is recommended for agentic workflows and complex tool calling scenarios").
//!
//! This module **plans**, it does not **execute**. Given a natural-language `goal`, it asks
//! glm-5.2 (with a fixed tool schema describing what this daemon can actually do) to propose an
//! ordered sequence of steps, and returns that sequence as human-readable strings
//! (`holoiroh_wire::ServerMessage::PlanReady::steps`) for the iOS client to show the user
//! *before* anything runs. Turning a plan step into real action is unchanged: a
//! `start_desktop_task` step still goes through [`crate::executor::ComputerUseExecutor`]
//! exactly as a typed/spoken prompt already does (see `control_channel.rs`'s existing
//! `ClientMessage::Prompt` handling) -- this module never calls `execute` itself, it only
//! *describes* the tool a caller would invoke to do so. That split is deliberate: it composes
//! over the seam ([`crate::executor::ComputerUseExecutor`]) from the outside, per that module's
//! own doc ("Swapping HoloDesktop for a different backend... means writing a second
//! `impl ComputerUseExecutor` -- the product code above the seam... never changes"), rather than
//! reaching into it.
//!
//! Direct-reqwest-with-bearer style, matching every other new `tinfoil_*` module.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::Duration;

use crate::tinfoil_models::{GLM_5_2, TINFOIL_BASE_URL};

const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

const PLANNER_SYSTEM_PROMPT: &str = "You are a task planner for a Mac-controlling desktop agent \
called Aro. Given a user's goal, break it into an ordered sequence of concrete steps using the \
tools provided. Each tool call you make becomes one plan step shown to the user before anything \
runs -- you are proposing a plan, not executing one, so call every tool you'd want run in the \
order you'd want it run, in a single response. If the goal is already a single simple action, \
propose exactly one step.";

/// The tools this daemon can actually perform, exposed to glm-5.2 exactly once here so every
/// caller shares the same schema (no duplicated tool-definition literals drifting apart).
/// Mirrors real capabilities only: `start_desktop_task` maps to
/// [`crate::executor::ComputerUseExecutor::execute`]; the other three map to
/// [`crate::tinfoil_documents::convert_documents`], [`crate::tinfoil_vision::analyze_image`],
/// and [`crate::tinfoil_audio::transcribe`] respectively. No tool here does anything this
/// daemon cannot already, honestly, do.
pub fn tool_schema() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "start_desktop_task",
                "description": "Have the Mac-controlling desktop agent (Holo3) perform an action on the Mac -- open an app, click something, type text, navigate a website, etc.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "instruction": {"type": "string", "description": "The natural-language instruction to hand to the desktop agent for this step."}
                    },
                    "required": ["instruction"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "process_document",
                "description": "Convert an attached document (PDF, DOCX, PPTX, XLSX, HTML, CSV) to markdown text so its content can be read or referenced.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": {"type": "string", "description": "Which attached document this step processes, in the user's own words."}
                    },
                    "required": ["description"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "analyze_image",
                "description": "Analyze an attached image and answer a question about it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": {"type": "string", "description": "What to determine about the image."}
                    },
                    "required": ["question"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "transcribe_audio",
                "description": "Transcribe an attached audio recording (the user's own voice) to text.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": {"type": "string", "description": "Which attached audio this step transcribes, in the user's own words."}
                    },
                    "required": ["description"]
                }
            }
        }
    ])
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize)]
struct ToolCall {
    function: ToolCallFunction,
}

#[derive(Deserialize)]
struct ToolCallFunction {
    name: String,
    /// Raw JSON-string arguments, per docs.tinfoil.sh/guides/tool-calling's documented
    /// response shape ("function.arguments: JSON string of parameters"). Parsed defensively --
    /// a malformed-JSON arguments string degrades this one step's description rather than
    /// failing the whole plan (see [`describe_step`]).
    arguments: String,
}

/// Plans `goal` into an ordered list of human-readable step descriptions. Returns `Result`
/// (not best-effort) -- like the other new Tinfoil modules, this is on the critical path of an
/// explicit user request.
pub async fn plan_task(api_key: &str, goal: &str) -> Result<Vec<String>> {
    let trimmed = goal.trim();
    if trimmed.is_empty() {
        bail!("plan_task called with an empty goal");
    }

    let body = serde_json::json!({
        "model": GLM_5_2,
        "messages": [
            {"role": "system", "content": PLANNER_SYSTEM_PROMPT},
            {"role": "user", "content": trimmed},
        ],
        "tools": tool_schema(),
        "tool_choice": "auto",
    });

    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(60),
        client
            .post(format!("{TINFOIL_BASE_URL}{CHAT_COMPLETIONS_PATH}"))
            .header("authorization", format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .json(&body)
            .send(),
    )
    .await
    .context("tinfoil planning request timed out")?
    .context("tinfoil planning request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!("tinfoil {CHAT_COMPLETIONS_PATH} (planner) returned {status}: {text}");
    }

    let parsed: ChatCompletionResponse = response
        .json()
        .await
        .context("failed to parse tinfoil planner response")?;
    let Some(choice) = parsed.choices.into_iter().next() else {
        bail!("tinfoil planner response had no choices");
    };

    if !choice.message.tool_calls.is_empty() {
        return Ok(choice
            .message
            .tool_calls
            .iter()
            .map(describe_step)
            .collect());
    }

    // No tool calls: the model answered in plain content instead (e.g. it judged the goal
    // needs no multi-step plan). A single-element plan of that content is still a truthful,
    // usable answer -- never silently return an empty plan for a non-empty goal.
    if !choice.message.content.trim().is_empty() {
        return Ok(vec![choice.message.content]);
    }

    bail!("tinfoil planner returned neither tool_calls nor content");
}

/// Renders one [`ToolCall`] as a human-readable plan step string.
fn describe_step(call: &ToolCall) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);

    match call.function.name.as_str() {
        "start_desktop_task" => {
            let instruction = args
                .get("instruction")
                .and_then(|v| v.as_str())
                .unwrap_or("(no instruction given)");
            format!("Desktop action: {instruction}")
        }
        "process_document" => {
            let desc = args
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("the attached document");
            format!("Read document: {desc}")
        }
        "analyze_image" => {
            let question = args
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("(no question given)");
            format!("Analyze image: {question}")
        }
        "transcribe_audio" => {
            let desc = args
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("the attached audio");
            format!("Transcribe audio: {desc}")
        }
        other => format!("{other}: {}", call.function.arguments),
    }
}
