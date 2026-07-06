// app/lib/evals/modelCompare.ts
//
// Head-to-head model comparison harness. Unlike the OpenAI-hosted evals
// (openai/flows.ts — can only sample OpenAI models), this runs candidate
// models through OUR runtime (resolveModel → OpenAI / Gemini / gateway
// vendors like Anthropic + Fable) on a small set of agentic micro-tasks that
// mirror the system's real failure modes:
//
//   tool-sequence   multi-step tool calling + connection-status judgment
//                   (the "hallucinated expired connection" + sheet-append case)
//   styled-doc      well-formatted styled content generation from structured
//                   JSON (the "populate a Google Doc from a stack" case)
//   email-extract   exact field extraction from a noisy Gmail-style payload
//                   (the automation prompt's event-identity case)
//
// Each (model × task) cell records a normal EvalRun in suite "model-compare"
// so results land in the existing /api/evals dashboard. Deterministic code
// checks grade what can be grepped; an LLM judge (gpt-5.4) scores quality.

import { generateText, generateObject, tool, stepCountIs } from "ai";
import { z } from "zod";

import { resolveModel } from "@/app/lib/modelRouting";
import { putCase, putRun, listRuns } from "@/app/lib/evals/store";
import type { GraderResult, EvalRun } from "@/app/lib/evals/types";

export const COMPARE_SUITE = "model-compare";

export const DEFAULT_COMPARE_MODELS = [
  "gpt-5.4",
  "gemini-3.1-pro-preview",
  "claude-opus-4.8",
  "fable-5",
];

// --- task definitions -----------------------------------------------------

export type TaskResult = {
  finalText: string;
  toolCalls: string[];
  durationMs: number;
};

// A judge request the batch path can defer: the rubric + the output to grade.
// `null` means the task is fully gradeable by deterministic code (no LLM judge).
export type JudgeSpec = { rubric: string; output: string };

export type CompareTask = {
  id: string;
  name: string;
  run: (modelName: string) => Promise<TaskResult>;
  // Deterministic, synchronous grades (string/JSON checks). No network.
  gradeCode: (r: TaskResult) => GraderResult[];
  // The LLM-judge portion, or null when the task needs no judge. Split out so
  // the batch path can submit just the judge calls to the OpenAI Batch API
  // while finalizing code grades immediately.
  judgeSpec: (r: TaskResult) => JudgeSpec | null;
};

// Derived synchronous-path grader: code checks + (optional) inline LLM judge.
async function gradeTask(task: CompareTask, r: TaskResult): Promise<GraderResult[]> {
  const grades = task.gradeCode(r);
  const spec = task.judgeSpec(r);
  if (spec) grades.push(await judge(spec.rubric, spec.output));
  return grades;
}

// System prompt + user prompt the judge uses — shared by the inline and batch
// paths so they grade identically.
export const JUDGE_SYSTEM =
  "You are a strict evaluator. Grade the OUTPUT against the RUBRIC. " +
  "pass=true only when the output genuinely satisfies every requirement.";

export function judgeUserPrompt(rubric: string, output: string): string {
  return `RUBRIC:\n${rubric}\n\nOUTPUT:\n${output.slice(0, 8000)}`;
}

// Parse a judge model's JSON into a GraderResult. Shared by inline + batch.
export function judgeResultFromJson(raw: unknown): GraderResult {
  const obj = (raw ?? {}) as { score?: unknown; pass?: unknown; notes?: unknown };
  const scoreNum = Number(obj.score);
  return {
    grader: "llm",
    name: "judge",
    pass: obj.pass === true,
    score: Number.isFinite(scoreNum) ? Math.max(0, Math.min(10, scoreNum)) / 10 : undefined,
    notes: typeof obj.notes === "string" ? obj.notes.slice(0, 300) : undefined,
  };
}

const STACK_JSON = JSON.stringify(
  [
    { name: "DNSSEC", category: "Security", purpose: "DNS integrity" },
    { name: "Cloudflare CDN", category: "Infrastructure", purpose: "Edge caching" },
    { name: "Next.js", category: "Framework", purpose: "App runtime" },
    { name: "Upstash Redis", category: "Data", purpose: "KV + queues" },
    { name: "Composio", category: "Integration", purpose: "SaaS tool calls" },
    { name: "Vercel WDK", category: "Orchestration", purpose: "Durable workflows" },
  ],
  null,
  2
);

const GMAIL_EVENT = {
  attachment_list: [],
  id: "19f0aa11223344cc",
  label_ids: ["UNREAD", "INBOX"],
  message_id: "19f0aa11223344cc",
  message_text: "Quarterly invoice attached. Total due: $4,120 by July 1.",
  message_timestamp: "2026-06-09T08:00:00Z",
  payload: {
    headers: [
      { name: "Delivered-To", value: "dylan@example.com" },
      { name: "ARC-Seal", value: "i=2; a=rsa-sha256; t=1780; cv=pass; b=JI3MUEI96gnBStNGax" },
      { name: "From", value: "billing@vendorcorp.io" },
      { name: "Subject", value: "Invoice #8841 — June" },
      { name: "Received", value: "by 2002:a05:7300:ca8 with SMTP id p40csp2072281dyk" },
    ],
  },
  thread_id: "19f0aa1122330000",
};

// LLM judge: scores a transcript against a rubric. Strong model, strict JSON.
async function judge(rubric: string, output: string): Promise<GraderResult> {
  try {
    const res = await generateObject({
      model: resolveModel("gpt-5.4"),
      schema: z.object({
        score: z.number().min(0).max(10),
        pass: z.boolean(),
        notes: z.string(),
      }),
      system: JUDGE_SYSTEM,
      prompt: judgeUserPrompt(rubric, output),
    });
    return judgeResultFromJson(res.object);
  } catch (err: any) {
    return {
      grader: "llm",
      name: "judge",
      pass: false,
      notes: `judge error: ${String(err?.message ?? err).slice(0, 200)}`,
    };
  }
}

function codeCheck(name: string, pass: boolean, notes?: string): GraderResult {
  return { grader: "code", name, pass, notes };
}

// Task 1: multi-step tool calling with a stale-labelled connection. The model
// must (a) proceed despite status=EXPIRED/stale:true, (b) read state, (c)
// append the NEW message id — not the one already in the sheet — and (d) not
// claim the connection expired.
function makeToolSequenceTask(): CompareTask {
  return {
    id: "mc_tool_sequence",
    name: "tool-sequence: sheet append with stale connection",
    run: async (modelName: string) => {
      const toolCalls: string[] = [];
      const tools = {
        check_integration_connected: tool({
          description: "Check whether a toolkit is connected for this user.",
          inputSchema: z.object({ toolkit: z.string() }),
          execute: async ({ toolkit }) => {
            toolCalls.push(`check_integration_connected(${toolkit})`);
            return {
              ok: true,
              toolkit,
              connected: true,
              status: "EXPIRED",
              stale: true,
              hint:
                "Account is connected but Composio labels it 'EXPIRED'. Composio refreshes these on use, so PROCEED and attempt the action normally. Do NOT tell the user it expired.",
            };
          },
        }),
        read_virtual_file: tool({
          description: "Read a file from the user's virtual filesystem.",
          inputSchema: z.object({ path: z.string() }),
          execute: async ({ path }) => {
            toolCalls.push(`read_virtual_file(${path})`);
            return {
              ok: true,
              content: JSON.stringify({ spreadsheetId: "ss_TEST123", sheetName: "Sheet1" }),
            };
          },
        }),
        COMPOSIO_EXECUTE_TOOL: tool({
          description: "Execute a Composio tool by slug with JSON arguments.",
          inputSchema: z.object({
            tool_slug: z.string(),
            arguments: z.string().describe("JSON-encoded arguments object"),
          }),
          execute: async ({ tool_slug, arguments: argsJson }) => {
            toolCalls.push(`COMPOSIO_EXECUTE_TOOL(${tool_slug}, ${argsJson.slice(0, 200)})`);
            if (/get|read|values/i.test(tool_slug) && !/append/i.test(tool_slug)) {
              return {
                ok: true,
                data: { values: [["ts", "from", "subject", "message_id"], ["2026-06-08", "old@x.com", "Old mail", "19eaa0d4b6c2e002"]] },
              };
            }
            if (/append/i.test(tool_slug)) {
              return { ok: true, data: { updatedRange: "Sheet1!A3:D3" } };
            }
            return { ok: true, data: {} };
          },
        }),
      };

      const t0 = Date.now();
      const out = await generateText({
        model: resolveModel(modelName),
        system:
          "You are an automation agent. PERFORM the task with your tools now. " +
          "Only a real 401/403 error from an actual tool call proves a connection " +
          "problem; a status label alone does not. When done, reply with a one-line " +
          "summary of what you did.",
        prompt:
          "CURRENT EVENT ID: 19f0bb5566778899\n" +
          "A new email arrived (from: alice@acme.com, subject: 'Renewal quote', " +
          "message_id: 19f0bb5566778899). Append one row [date, from, subject, message_id] " +
          "for THIS email to the Google Sheet whose id is stored in /automations/a_test/state.json. " +
          "Skip only if THIS message_id is already in the sheet. Start by checking the " +
          "googlesheets connection.",
        tools,
        stopWhen: stepCountIs(8),
      });
      return { finalText: out.text, toolCalls, durationMs: Date.now() - t0 };
    },
    gradeCode: (r) => {
      const calls = r.toolCalls.join("\n");
      const appended = /COMPOSIO_EXECUTE_TOOL\([^)]*append/i.test(calls) && calls.includes("19f0bb5566778899");
      const noExpiryClaim = !/expired|reconnect|re-authorize|reauthorize|disconnected/i.test(r.finalText);
      const proceeded = /COMPOSIO_EXECUTE_TOOL/.test(calls);
      return [
        codeCheck("appended_current_id", appended, appended ? undefined : `tool calls: ${calls.slice(0, 300)}`),
        codeCheck("no_expiry_hallucination", noExpiryClaim, noExpiryClaim ? undefined : `final: ${r.finalText.slice(0, 200)}`),
        codeCheck("proceeded_despite_stale", proceeded),
      ];
    },
    judgeSpec: (r) => ({
      rubric:
        "The agent had to append a row for message_id 19f0bb5566778899 to the sheet and summarize plainly. Full marks only if it appended exactly one row for the NEW id, did not claim the connection expired, and the summary is short and accurate.",
      output: `TOOL CALLS:\n${r.toolCalls.join("\n")}\n\nFINAL:\n${r.finalText}`,
    }),
  };
}

// Task 2: styled content generation from structured JSON.
function makeStyledDocTask(): CompareTask {
  return {
    id: "mc_styled_doc",
    name: "styled-doc: stack report from JSON",
    run: async (modelName: string) => {
      const t0 = Date.now();
      const out = await generateText({
        model: resolveModel(modelName),
        system:
          "You write polished, well-structured markdown documents. No preamble — output the document only.",
        prompt:
          `Turn this technology stack JSON into a professional markdown report titled "Platform Stack Overview". ` +
          `Requirements: an intro paragraph; a markdown table with columns Name | Category | Purpose covering EVERY item; ` +
          `a "By category" section with one bolded subsection per distinct category listing its items; ` +
          `and a closing one-sentence summary.\n\n${STACK_JSON}`,
      });
      return { finalText: out.text, toolCalls: [], durationMs: Date.now() - t0 };
    },
    gradeCode: (r) => {
      const names = ["DNSSEC", "Cloudflare CDN", "Next.js", "Upstash Redis", "Composio", "Vercel WDK"];
      const allPresent = names.every((n) => r.finalText.includes(n));
      const hasTable = /\|\s*Name\s*\|/i.test(r.finalText) && (r.finalText.match(/\n\|/g) ?? []).length >= 6;
      return [
        codeCheck("all_items_present", allPresent),
        codeCheck("has_complete_table", hasTable),
      ];
    },
    judgeSpec: (r) => ({
      rubric:
        "A professional markdown report titled 'Platform Stack Overview' with an intro paragraph, a complete Name|Category|Purpose table covering all six items, a 'By category' section with bolded subsections per category, and a one-sentence closing summary. Grade formatting quality, completeness, and adherence strictly.",
      output: r.finalText,
    }),
  };
}

// Task 3: exact field extraction from a noisy Gmail-style payload.
function makeEmailExtractTask(): CompareTask {
  return {
    id: "mc_email_extract",
    name: "email-extract: identity fields from noisy payload",
    run: async (modelName: string) => {
      const t0 = Date.now();
      const out = await generateText({
        model: resolveModel(modelName),
        system: "Reply with ONLY a JSON object. No markdown fences, no commentary.",
        prompt:
          `From this Gmail webhook payload, extract exactly: {"id": <message id>, "threadId": <thread id>, "from": <sender address>, "subject": <subject>, "snippet": <first 60 chars of body text>}.\n\n` +
          JSON.stringify(GMAIL_EVENT, null, 2),
      });
      return { finalText: out.text, toolCalls: [], durationMs: Date.now() - t0 };
    },
    gradeCode: (r) => {
      let parsed: any = null;
      try {
        parsed = JSON.parse(r.finalText.replace(/^```(?:json)?\s*/i, "").replace(/```\s*$/, ""));
      } catch {
        /* leave null */
      }
      return [
        codeCheck("valid_json", parsed !== null, parsed === null ? `raw: ${r.finalText.slice(0, 150)}` : undefined),
        codeCheck("correct_id", parsed?.id === "19f0aa11223344cc", parsed ? `id=${parsed.id}` : undefined),
        codeCheck("correct_thread", parsed?.threadId === "19f0aa1122330000"),
        codeCheck("correct_from", typeof parsed?.from === "string" && parsed.from.includes("billing@vendorcorp.io")),
        codeCheck("correct_subject", typeof parsed?.subject === "string" && parsed.subject.includes("Invoice #8841")),
      ];
    },
    judgeSpec: () => null,
  };
}

export const COMPARE_TASKS: CompareTask[] = [
  makeToolSequenceTask(),
  makeStyledDocTask(),
  makeEmailExtractTask(),
];

// --- runner ---------------------------------------------------------------

export function getCompareTask(taskId: string): CompareTask | undefined {
  return COMPARE_TASKS.find((t) => t.id === taskId);
}

export async function ensureCompareCase(task: CompareTask): Promise<void> {
  await putCase({
    id: task.id,
    suite: COMPARE_SUITE,
    name: task.name,
    graders: [],
    createdAt: Date.now(),
  });
}

// Persist a finished (model × task) cell. Used by both the synchronous runner
// and the batch path (which assembles `grades` from code checks + a deferred
// judge before calling this).
export async function finalizeCell(args: {
  model: string;
  task: CompareTask;
  result: TaskResult;
  grades: GraderResult[];
  errorMessage?: string;
}): Promise<EvalRun> {
  const { model, task, result, grades, errorMessage } = args;
  const passCount = grades.filter((g) => g.pass).length;
  const status: EvalRun["status"] = errorMessage
    ? "error"
    : grades.length > 0 && passCount === grades.length
      ? "pass"
      : passCount > 0
        ? "partial"
        : "fail";

  return putRun({
    caseId: task.id,
    suite: COMPARE_SUITE,
    input: { goal: task.name, meta: { model } },
    actual: {
      finalText: result.finalText.slice(0, 4000),
      toolCalls: result.toolCalls,
      artifactPaths: [],
      durationMs: result.durationMs,
      errorMessage,
    },
    grades,
    status,
  });
}

export async function runCompareCell(args: {
  model: string;
  taskId: string;
}): Promise<EvalRun> {
  const task = getCompareTask(args.taskId);
  if (!task) throw new Error(`unknown compare task: ${args.taskId}`);

  await ensureCompareCase(task);

  let result: TaskResult;
  let grades: GraderResult[];
  let errorMessage: string | undefined;
  try {
    result = await task.run(args.model);
    grades = await gradeTask(task, result);
  } catch (err: any) {
    result = { finalText: "", toolCalls: [], durationMs: 0 };
    errorMessage = String(err?.message ?? err).slice(0, 400);
    grades = [codeCheck("executed", false, errorMessage)];
  }

  return finalizeCell({ model: args.model, task, result, grades, errorMessage });
}

// Aggregate recent model-compare runs into a per-model scoreboard.
export async function compareSummary(): Promise<
  Array<{
    model: string;
    cells: number;
    passed: number;
    partial: number;
    failed: number;
    avgJudgeScore: number | null;
    avgDurationMs: number | null;
    byTask: Record<string, string>;
  }>
> {
  const runs = await listRuns({ suite: COMPARE_SUITE, limit: 200 });
  // Keep only the newest run per (model, task).
  const newest = new Map<string, EvalRun>();
  for (const r of runs) {
    const model = String(r.input.meta?.model ?? "?");
    const key = `${model}::${r.caseId}`;
    if (!newest.has(key)) newest.set(key, r); // listRuns returns newest first
  }
  const byModel = new Map<string, EvalRun[]>();
  for (const [key, r] of newest) {
    const model = key.split("::")[0];
    if (!byModel.has(model)) byModel.set(model, []);
    byModel.get(model)!.push(r);
  }
  const out = [];
  for (const [model, cells] of byModel) {
    const judgeScores = cells
      .flatMap((c) => c.grades.filter((g) => g.name === "judge" && typeof g.score === "number"))
      .map((g) => g.score as number);
    const durations = cells
      .map((c) => c.actual.durationMs)
      .filter((d): d is number => typeof d === "number" && d > 0);
    const byTask: Record<string, string> = {};
    for (const c of cells) {
      const p = c.grades.filter((g) => g.pass).length;
      byTask[c.caseId] = `${c.status} (${p}/${c.grades.length})`;
    }
    out.push({
      model,
      cells: cells.length,
      passed: cells.filter((c) => c.status === "pass").length,
      partial: cells.filter((c) => c.status === "partial").length,
      failed: cells.filter((c) => c.status === "fail" || c.status === "error").length,
      avgJudgeScore: judgeScores.length
        ? Math.round((judgeScores.reduce((a, b) => a + b, 0) / judgeScores.length) * 100) / 100
        : null,
      avgDurationMs: durations.length
        ? Math.round(durations.reduce((a, b) => a + b, 0) / durations.length)
        : null,
      byTask,
    });
  }
  out.sort((a, b) => (b.avgJudgeScore ?? 0) - (a.avgJudgeScore ?? 0));
  return out;
}
