// app/steps/codeInterpreterStep.ts
//
// Subtask kind for the deep-mode orchestrator: hand a problem to OpenAI's
// hosted Python container ("code_interpreter" tool on the Responses API).
// Good fit for:
//   - numeric / symbolic computation
//   - data wrangling that benefits from a real interpreter
//   - quick plot / chart generation
//   - parsing / transforming structured files
//
// The Responses API spawns an ephemeral container per call when
// `container: { type: "auto" }` — we don't manage container lifecycle
// here. Output (stdout text, file references) is folded back into the
// orchestrator's accumulated subtaskResults.
//
// Model: env `CODE_INTERPRETER_MODEL` (default gpt-4.1 — supports the
// code_interpreter tool, cheap enough for routine compute).
//
// Failure modes that are normal:
//   - Container provisioning errors → return ok:false with details; the
//     orchestrator can re-plan and try a different action.
//   - Long-running code that exceeds the API timeout → same treatment.

import OpenAI from "openai";

import { env } from "@/app/lib/env";
import { appendThought } from "@/app/lib/jobStore";
import { recordCost } from "@/app/lib/costTracker";

let openaiClient: OpenAI | null = null;
function getOpenAI(): OpenAI {
  if (!openaiClient) {
    openaiClient = new OpenAI({ apiKey: env("OPENAI_API_KEY") });
  }
  return openaiClient;
}

const APPROX_INPUT_TOKENS = 3_000;
const APPROX_OUTPUT_TOKENS = 1_500;

export type CodeInterpreterResult = {
  ok: boolean;
  text: string;
  fileRefs: string[]; // OpenAI file ids the container produced
  error?: string;
};

export async function codeInterpreterStep(args: {
  jobId: string;
  goal: string;
  instructions: string;
}): Promise<CodeInterpreterResult> {
  "use step";

  const modelName = env("CODE_INTERPRETER_MODEL") ?? "gpt-4.1";

  await appendThought(args.jobId, {
    kind: "tool",
    text: `code_interpreter: start (model=${modelName})`,
    data: { goal: args.goal },
  });

  const client = getOpenAI();
  // Compose the prompt: orchestrator sets `goal` (what we want) and
  // `instructions` (how to approach it / what shape the answer takes).
  // The model decides what Python to run inside the container.
  const userPrompt = [
    `Goal: ${args.goal}`,
    "",
    args.instructions
      ? `Instructions:\n${args.instructions}`
      : "",
    "",
    "Use the code interpreter to run Python in a sandboxed container.",
    "Return concrete results (numbers, tables, file references), not just",
    "code. Keep the final answer terse and structured.",
  ]
    .filter(Boolean)
    .join("\n");

  let response: any;
  try {
    response = await (client as any).responses.create({
      model: modelName,
      tools: [
        {
          type: "code_interpreter",
          container: { type: "auto" },
        },
      ],
      input: userPrompt,
    });
  } catch (err: any) {
    const msg = err?.message ?? String(err);
    await appendThought(args.jobId, {
      kind: "error",
      text: `code_interpreter failed: ${msg.slice(0, 200)}`,
    });
    return { ok: false, text: "", fileRefs: [], error: msg };
  }

  // Extract the model's final text + any file refs produced inside the
  // container. The Responses API surfaces these in response.output as
  // typed items.
  let finalText = "";
  const fileRefs: string[] = [];

  const items: any[] = Array.isArray(response?.output) ? response.output : [];
  for (const item of items) {
    if (item?.type === "message") {
      const parts = Array.isArray(item.content) ? item.content : [];
      for (const p of parts) {
        if (p?.type === "output_text" && typeof p.text === "string") {
          finalText += (finalText ? "\n" : "") + p.text;
        }
      }
    }
    // code_interpreter tool emits container file references on completion.
    // The exact shape varies across SDK versions — be tolerant.
    if (
      item?.type === "code_interpreter_call" ||
      item?.type === "tool_call_output" ||
      item?.type === "code_interpreter_output"
    ) {
      const outputs = Array.isArray(item.outputs) ? item.outputs : [];
      for (const o of outputs) {
        if (o?.type === "container_file_citation" || o?.type === "file") {
          if (typeof o.file_id === "string") fileRefs.push(o.file_id);
        }
      }
    }
  }

  if (!finalText) {
    // Fallback: some SDK versions expose a flat output_text helper.
    if (typeof (response as any)?.output_text === "string") {
      finalText = (response as any).output_text;
    }
  }

  // Record cost approximation. The Responses API's usage object varies;
  // be defensive.
  const usage = (response as any)?.usage ?? null;
  await recordCost({
    jobId: args.jobId,
    model: modelName,
    usage: usage
      ? {
          inputTokens: usage.input_tokens ?? usage.prompt_tokens,
          outputTokens: usage.output_tokens ?? usage.completion_tokens,
        }
      : { inputTokens: APPROX_INPUT_TOKENS, outputTokens: APPROX_OUTPUT_TOKENS },
  });

  await appendThought(args.jobId, {
    kind: "result",
    text: `code_interpreter: done — ${finalText.length} chars${fileRefs.length ? `, ${fileRefs.length} file(s)` : ""}`,
    data: { fileRefs },
  });

  return { ok: true, text: finalText, fileRefs };
}
