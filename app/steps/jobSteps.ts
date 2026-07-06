// app/steps/jobSteps.ts
//
// WDK durable steps used by jobWorkflow. Each function is a `"use step"`,
// which means Workflow DevKit memoizes its result keyed by call site + args
// across function-instance boundaries (cron-triggered resumptions, fluid
// re-invocations, etc.).
//
// Slice 1: the only step that does real work is `executeAgentTurnStep`. The
// others (`clarifyStep`, `planStep`, `verifyStep`) are deliberate, named
// stubs that produce pass-through outputs. Slice 2/3 will replace their
// bodies with real subagent calls without touching the machine graph or
// workflow runner.

import { generateObject, type ModelMessage } from "ai";
import { z } from "zod/v4";

import { agentTurn } from "@/app/steps/agentTurn";
import type { SubAgentScope } from "@/app/lib/agents";
import { loadHistoryStep, saveHistoryStep } from "@/app/steps/sessionStateSteps";
import { recordSolution } from "@/app/lib/solutionMemory";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import type { Channel } from "@/app/lib/identity";
import {
  appendThought,
  updateJobMeta,
  addArtifact,
  getJobMeta as _getJobMeta,
  getThoughts,
  loadSnapshot as _loadSnapshot,
  saveSnapshot as _saveSnapshot,
  type JobMeta,
  type JobStatus,
  type Thought,
} from "@/app/lib/jobStore";
import { recordAudit } from "@/app/lib/auditLog";
import { recordJobEval } from "@/app/lib/evals/recorder";
import type { PlanStep } from "@/app/machines/jobMachine";
import { buildLlmArgs } from "@/app/lib/modelRouting";
import {
  detectModality,
  rubricFor,
  findHardFails,
  type ModalityId,
} from "@/app/lib/rubrics";
import { getStore } from "@/app/lib/store";
import { recordCost, getJobCost, type JobCost } from "@/app/lib/costTracker";
import { env } from "@/app/lib/env";

// Parse a non-negative integer (milliseconds) from env, falling back to a
// default when unset/blank/invalid. Used for tunable per-attempt deadlines.
function intFromEnvMs(name: string, fallback: number): number {
  const raw = env(name);
  if (raw == null || raw === "") return fallback;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : fallback;
}

const MODALITY_IDS = [
  "code-rust",
  "code-rust-zk",
  "code-ui-nextjs-ts",
  "code-generic",
  "latex-pdf",
  "research",
  "generic",
] as const satisfies readonly ModalityId[];

// ----------------------------------------------------------------------------
// Store-access steps
//
// Vercel Workflow's VM forbids `setTimeout`, which the Upstash redis client
// uses internally. Workflow functions therefore must NEVER touch the store
// directly — they call these `"use step"` wrappers, which run in the normal
// Node runtime where Upstash works fine.
// ----------------------------------------------------------------------------

export async function loadJobMetaStep(jobId: string): Promise<JobMeta | null> {
  "use step";
  return _getJobMeta(jobId);
}

// Cooperative-cancel probe: true once /stop (or /start) has marked this job
// cancelled. The job workflow calls this between ticks and halts when it fires.
export async function isJobCancelledStep(jobId: string): Promise<boolean> {
  "use step";
  const meta = await _getJobMeta(jobId);
  return meta?.status === "cancelled";
}

export async function loadJobSnapshotStep(
  jobId: string
): Promise<unknown | null> {
  "use step";
  return _loadSnapshot(jobId);
}

export async function saveJobSnapshotStep(args: {
  jobId: string;
  snapshot: unknown;
}): Promise<void> {
  "use step";
  await _saveSnapshot(args.jobId, args.snapshot);
}

export async function markJobStartedStep(args: {
  jobId: string;
}): Promise<void> {
  "use step";
  const meta = await _getJobMeta(args.jobId);
  if (!meta) return;
  if (meta.startedAt) return;
  await updateJobMeta(args.jobId, { startedAt: Date.now() });
}

// Read accumulated cost so the workflow / orchestrator can make budget-aware
// decisions. Wrapped as a step so the workflow VM doesn't touch the store.
export async function getJobCostStep(jobId: string): Promise<JobCost> {
  "use step";
  return getJobCost(jobId);
}

// ----------------------------------------------------------------------------
// Status / thought logging
// ----------------------------------------------------------------------------

export async function logTransitionStep(args: {
  jobId: string;
  status: JobStatus;
  text: string;
  data?: Record<string, unknown>;
}): Promise<void> {
  "use step";
  await updateJobMeta(args.jobId, { status: args.status });
  await appendThought(args.jobId, {
    kind: "transition",
    text: args.text,
    data: args.data,
  });
}

export async function logThoughtStep(args: {
  jobId: string;
  kind: "info" | "reasoning" | "tool" | "observation" | "error" | "result";
  text: string;
  data?: Record<string, unknown>;
}): Promise<void> {
  "use step";
  await appendThought(args.jobId, {
    kind: args.kind,
    text: args.text,
    data: args.data,
  });
}

// ----------------------------------------------------------------------------
// Phase steps
// ----------------------------------------------------------------------------

// Clarifier — strictly infer-and-proceed.
//
// The clarifier never asks the user a question. Its only job is to surface
// the assumptions the agent is making so the user can audit them via /ask
// or the thought log. If the prompt is vague, the clarifier infers the
// most-likely intent and records that as an assumption rather than blocking
// for input. (The machine still has a `needs_input` state for future use
// — slice 4+ may add an explicit `/clarify <jobId>` flow — but no normal
// job ever lands there today.)
const clarifySchema = z.object({
  // Short bullet phrases capturing every non-obvious choice the agent will
  // make when interpreting the prompt. The user reads these via /ask.
  assumptions: z.array(z.string()).max(8),
  // The clarifier's read of how clear the prompt is. Diagnostic only —
  // does NOT gate progress. Useful for telemetry / future tuning.
  clarity: z.enum(["clear", "mostly_clear", "ambiguous"]),
});

export async function clarifyStep(args: {
  jobId: string;
  prompt: string;
}): Promise<{ assumptions: string[] }> {
  "use step";

  const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0.2 });

  const system = [
    "You are the clarifier for an autonomous agent. The agent will execute the",
    "user's request without asking back-and-forth questions. Your job is to",
    "infer the user's most likely intent and surface the assumptions you are",
    "making so they are visible in the agent's reasoning log.",
    "",
    "Rules:",
    "1. NEVER request more information from the user. Always assume.",
    "2. When the prompt is vague, pick the most-likely interpretation given",
    "   typical user intent and list every non-obvious choice as an assumption.",
    "3. When the prompt is detailed, your assumption list may be short or",
    "   empty — only list things the user did NOT explicitly specify.",
    "4. Keep each assumption to one short sentence. The user should be able to",
    "   skim them quickly.",
    "",
    "Output JSON:",
    "  assumptions: array of short assumption strings (may be empty)",
    "  clarity:     'clear' | 'mostly_clear' | 'ambiguous' — diagnostic only",
  ].join("\n");

  try {
    const result = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: clarifySchema,
      system,
      prompt: `User request:\n${args.prompt}`,
    });
    const { object } = result;
    await recordCost({
      jobId: args.jobId,
      model: llm.modelName,
      usage: (result as any).usage,
    });

    await appendThought(args.jobId, {
      kind: "reasoning",
      text:
        object.assumptions.length === 0
          ? `clarify: no assumptions needed (clarity=${object.clarity})`
          : `clarify: proceeding with ${object.assumptions.length} assumption(s) (clarity=${object.clarity})`,
      data: { assumptions: object.assumptions, clarity: object.clarity },
    });

    return { assumptions: object.assumptions };
  } catch (err: any) {
    // Clarifier should never block the pipeline — if the model misbehaves we
    // proceed with no recorded assumptions.
    await appendThought(args.jobId, {
      kind: "error",
      text: `clarify failed (degraded to no-op): ${err?.message ?? String(err)}`,
    });
    return { assumptions: [] };
  }
}

// Real planner. Reasoning model if REASONING_MODEL_NAME is set, else smart.
// Emits a structured plan AND classifies modality so the verifier picks the
// right rubric. On revise passes, verifier notes are folded in.
const planSchema = z.object({
  // Modality classification — drives the verifier rubric choice.
  modality: z.enum(MODALITY_IDS),
  // Ordered list of plan steps. Slice 2: descriptive only; agentTurn does the
  // execution. Slice 3+ may execute them as separate states.
  plan: z
    .array(
      z.object({
        id: z.string(),
        kind: z.enum(["research", "code", "write", "tool", "verify"]),
        description: z.string(),
      })
    )
    .min(1)
    .max(12),
  // Short rationale; logged to the thought stream so users can audit the plan.
  rationale: z.string(),
});

export async function planStep(args: {
  jobId: string;
  prompt: string;
  assumptions: string[];
  verifierNotes?: string[];
}): Promise<{ plan: PlanStep[]; modality: ModalityId }> {
  "use step";

  const llm = buildLlmArgs({ purpose: "reasoning", temperature: 0.3 });

  const fallbackModality = detectModality(args.prompt);

  const system = [
    "You are a senior planner for an autonomous agent that must produce",
    "high-fidelity, fully functional output — never skeletons, stubs, or",
    "placeholders.",
    "",
    "Output a structured plan:",
    "  modality — what kind of output is being produced. Choose from:",
    `             ${MODALITY_IDS.join(", ")}.`,
    "  plan     — ordered concrete steps; each step's 'description' should be",
    "             specific enough that an executor knows what to do.",
    "  rationale — one paragraph on why this plan, in your own voice.",
    "",
    "If verifier notes are provided, treat them as MUST-FIX feedback from a",
    "critic that already reviewed a prior attempt. The new plan must address",
    "every note.",
    "",
    "SCOPE EXACTNESS: the plan must cover the request completely and cover",
    "ONLY the request — every user-stated requirement maps to a step, and no",
    "step introduces deliverables, tools, or side effects the user didn't ask",
    "for. When the request is ambiguous, plan the accepted assumptions — do",
    "not widen scope to hedge against them.",
  ].join("\n");

  const userPrompt = [
    `User request:\n${args.prompt}`,
    args.assumptions.length
      ? `\nAgent assumptions (already accepted):\n- ${args.assumptions.join("\n- ")}`
      : "",
    args.verifierNotes?.length
      ? `\nVerifier notes from prior attempt — MUST FIX:\n- ${args.verifierNotes.join("\n- ")}`
      : "",
  ]
    .filter(Boolean)
    .join("\n");

  try {
    const result = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: planSchema,
      system,
      prompt: userPrompt,
    });
    const { object } = result;
    await recordCost({
      jobId: args.jobId,
      model: llm.modelName,
      usage: (result as any).usage,
    });

    await appendThought(args.jobId, {
      kind: "reasoning",
      text: `plan: ${object.plan.length} step(s) for modality=${object.modality}`,
      data: {
        modality: object.modality,
        plan: object.plan,
        rationale: object.rationale,
        modelTier: "reasoning",
      },
    });

    return { plan: object.plan, modality: object.modality };
  } catch (err: any) {
    // Don't block on planner failure — fall back to a trivial single-step plan
    // and heuristic modality so the job can still finish.
    await appendThought(args.jobId, {
      kind: "error",
      text: `plan failed (degraded to single-step): ${err?.message ?? String(err)}`,
    });
    return {
      plan: [
        {
          id: "p1",
          kind: "tool",
          description: "Run a single agent turn over the conversation history.",
        },
      ],
      modality: fallbackModality,
    };
  }
}

// VFS path set is namespaced by tenantId (channel-qualified userId) + sessionId
// in agentTurn.ts. We diff it before/after the turn to capture artifacts the
// agent created without coupling jobSteps to agentTurn's internals.
function vfsPathsKey(tenantId: string, sessionId: string): string {
  return `vfs:${tenantId}:${sessionId}:paths`;
}

// Real worker: runs agentTurn() inside a WDK step, persists history, returns
// the assistant text and the set of VFS paths created during this turn.
export async function executeAgentTurnStep(args: {
  jobId: string;
  tenantId: string;
  sessionId: string;
  channel: Channel;
  prompt: string;
  showTyping: boolean;
  // Optional model override — used to route code modalities to the coding
  // model and to let the deep-mode orchestrator pick per-subtask models.
  modelName?: string;
  // Sub-agent scope: run this turn AS a named, toolkit-scoped agent persona
  // (workforce member turns). See app/lib/agents.ts.
  agent?: SubAgentScope;
}): Promise<{ text: string; delivered: boolean; artifacts: string[] }> {
  "use step";

  const store = getStore();
  const vfsKey = vfsPathsKey(args.tenantId, args.sessionId);
  const before = new Set(await store.smembers(vfsKey));

  const existing = (await loadHistoryStep(args.sessionId)) as ModelMessage[];
  const history: ModelMessage[] = Array.isArray(existing) ? [...existing] : [];

  // Append the user prompt that kicked off this job.
  history.push({ role: "user", content: args.prompt });

  await appendThought(args.jobId, {
    kind: "tool",
    text: "agentTurn: start",
    data: { historyLen: history.length },
  });

  // Sum input text length for cost estimation (agentTurn doesn't return
  // usage). We approximate from char counts — coarse but enough for budgeting.
  const inputApprox = history
    .map((m) => {
      const c = (m as any).content;
      if (typeof c === "string") return c;
      if (Array.isArray(c))
        return c.map((p: any) => p?.text ?? "").join(" ");
      return "";
    })
    .join("\n");

  // Per-attempt deadline so a stuck agent (rate-limited Composio call,
  // hung browser tool, context-window-exhausted retry loop) doesn't burn
  // the entire WDK step budget. On expiry we throw a tagged error so the
  // retry log (and the per-attempt error logging below) make it obvious
  // WHY the step keeps retrying instead of the opaque "exceeded max
  // retries" the user saw on job j_486875b83384.
  //
  // Vercel Functions now run up to 30 min (1800s) on Fluid Compute, and the
  // WDK marks step functions `maxDuration: max`, so a long multi-tool deep
  // job can legitimately take much longer than the old 8-min cap allowed.
  // Default raised to 20 min — env-overridable.
  //
  // IMPORTANT: this deadline MUST stay BELOW the actual function budget so
  // OUR timer fires (clean tagged error → orchestrator recovers) instead of
  // the platform killing the function uncatchably (which WDK then retries
  // into the same hang — see orchestrateStep). The function budget = the
  // project's configured max function duration: 800s is the Fluid default;
  // raising it toward 1800s requires bumping the project's max duration in
  // the Vercel dashboard. If your project max is still 800s, set
  // AGENT_TURN_DEADLINE_MS=780000 so our timer wins.
  const PER_ATTEMPT_DEADLINE_MS = intFromEnvMs(
    "AGENT_TURN_DEADLINE_MS",
    20 * 60 * 1000
  );
  const attemptStart = Date.now();

  let result: Awaited<ReturnType<typeof agentTurn>>;
  try {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(
        () => reject(
          new Error(
            `agentTurn deadline ${Math.round(PER_ATTEMPT_DEADLINE_MS / 1000)}s exceeded`
          )
        ),
        PER_ATTEMPT_DEADLINE_MS
      );
    });
    try {
      result = await Promise.race([
        agentTurn({
          sessionId: args.sessionId,
          userId: args.tenantId,
          channel: args.channel,
          history,
          showTyping: args.showTyping,
          modelName: args.modelName,
          jobId: args.jobId,
          agent: args.agent,
        }),
        deadline,
      ]);
    } finally {
      if (timer) clearTimeout(timer);
    }
  } catch (err: any) {
    const elapsedMs = Date.now() - attemptStart;
    const msg = err?.message ?? String(err);
    // Surface the real error in the job's thought log so the
    // dashboard / `/job status` shows what went wrong per attempt
    // instead of just "exceeded max retries (4 retries)" at the end.
    await appendThought(args.jobId, {
      kind: "error",
      text: `agentTurn attempt failed after ${Math.round(elapsedMs / 1000)}s: ${msg.slice(0, 400)}`,
      data: {
        modelName: args.modelName,
        historyLen: history.length,
        errorClass: err?.name ?? "Error",
      },
    });
    // Re-throw so WDK's retry logic kicks in. If it's the last retry,
    // the orchestrator catches it and the job ends up failed —
    // appropriate semantics, but now the user can see the actual
    // error per attempt instead of an opaque retry-exhaustion.
    throw err;
  }

  // Record estimated cost. The model used by agentTurn is either the override
  // we passed, or the env-driven default; safe to attribute to args.modelName.
  await recordCost({
    jobId: args.jobId,
    model: args.modelName ?? "gpt-5.4",
    usage: null,
    promptText: inputApprox,
    outputText: result.text ?? "",
  });

  history.push({ role: "assistant", content: result.text });
  await saveHistoryStep(args.sessionId, history);

  // Diff VFS to surface new artifact paths.
  const after = await store.smembers(vfsKey);
  const created: string[] = [];
  for (const p of after) {
    if (!before.has(p)) created.push(p);
  }
  for (const p of created) {
    await addArtifact(args.jobId, p);
  }

  await appendThought(args.jobId, {
    kind: "result",
    text: "agentTurn: complete",
    data: {
      textChars: result.text?.length ?? 0,
      delivered: Boolean((result as any).delivered),
      newArtifacts: created.length,
    },
  });

  return {
    text: result.text ?? "",
    delivered: Boolean((result as any).delivered),
    artifacts: created,
  };
}
// WDK retries this step up to 4 times by default. When an agentTurn
// hangs (timing out at our per-attempt deadline) or fails for a persistent
// reason (rate limit, context-window overflow, downstream API down),
// retrying 4× × backoff burned >2 hours on j_486875b83384 with no
// useful work done. Cut to 1 retry so the orchestrator gets control
// back quickly and can either revise its plan or finalize with what
// it has — turn-level errors are almost never transient at this layer.
(executeAgentTurnStep as unknown as { maxRetries?: number }).maxRetries = 1;

// Real critic. Two-stage gate:
//   1) Cheap regex hardFails from the modality rubric — if any hit, auto-revise
//      without spending a model call.
//   2) Reasoning-model judgement against the rubric's criteria list. Returns
//      structured pass/notes.
//
// This is the "no half-ass" gate. It's intentionally strict: notes that fail
// to address a criterion cause a revise pass (up to MAX_REVISE_PASSES).
const verifySchema = z.object({
  pass: z.boolean(),
  notes: z.array(z.string()).max(12),
  // The critic's own confidence — helps debugging.
  confidence: z.enum(["low", "medium", "high"]),
});

export async function verifyStep(args: {
  jobId: string;
  prompt: string;
  resultText: string;
  modality: ModalityId;
}): Promise<{
  pass: boolean;
  notes: string[];
  confidence: "low" | "medium" | "high";
}> {
  "use step";

  const rubric = rubricFor(args.modality);

  // Stage 1: cheap regex pre-filter.
  const hardFails = findHardFails(rubric, args.resultText);
  if (hardFails.length) {
    await appendThought(args.jobId, {
      kind: "observation",
      text: `verify: hard-fail (${hardFails.length}) — revise without model call`,
      data: { hardFails, modality: rubric.id },
    });
    // A regex hard-fail is an unambiguous miss — treat as high-confidence fail.
    return { pass: false, notes: hardFails, confidence: "high" };
  }

  // Stage 2: reasoning-model critic.
  const llm = buildLlmArgs({ purpose: "reasoning", temperature: 0.2 });

  const system = [
    `You are a strict critic for ${rubric.label} output. Apply the criteria`,
    "below to the agent's result. Every criterion must be satisfied for pass.",
    "If even one criterion is unmet, return pass=false with concrete,",
    "actionable notes — not generic feedback. Each note should tell the",
    "planner exactly what to change.",
    "",
    "Do not pass output that is a skeleton, stub, abbreviated example, or",
    "'here's how you would do it' write-up. The user expects a complete,",
    "production-grade artifact.",
    "",
    "EVIDENCE GROUNDING (applies before all other criteria): any claim of a",
    "performed side effect (email sent, row appended, doc created, event",
    "scheduled) must be corroborated by concrete evidence in the result —",
    "resource ids, links, or explicit tool outcomes. A result that asserts",
    "success while its own narrative shows the underlying call failed, was",
    "skipped, or never happened is FABRICATED: return pass=false with a note",
    "naming the unsupported claim. Likewise fail results that dress an error",
    "up as completion ('unable to X, but consider it done').",
    "",
    "Criteria:",
    ...rubric.criteria.map((c, i) => `  ${i + 1}. ${c}`),
  ].join("\n");

  const userPrompt = [
    `Original request:\n${args.prompt}`,
    "",
    `Agent result:\n${args.resultText}`,
  ].join("\n");

  try {
    const result = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: verifySchema,
      system,
      prompt: userPrompt,
    });
    const { object } = result;
    await recordCost({
      jobId: args.jobId,
      model: llm.modelName,
      usage: (result as any).usage,
    });

    await appendThought(args.jobId, {
      kind: "observation",
      text: object.pass
        ? `verify: PASS (confidence=${object.confidence}, modality=${rubric.id})`
        : `verify: REVISE — ${object.notes.length} note(s)`,
      data: { notes: object.notes, modality: rubric.id, confidence: object.confidence },
    });

    return { pass: object.pass, notes: object.notes, confidence: object.confidence };
  } catch (err: any) {
    // Don't get stuck in a verify-loop if the critic itself is broken.
    // Treat critic failure as a pass (better to ship than to spin) but log it.
    await appendThought(args.jobId, {
      kind: "error",
      text: `verify failed (degraded to auto-pass): ${err?.message ?? String(err)}`,
    });
    // Critic infra failure, not a quality signal — return "medium" so the
    // low-confidence extra-pass rule doesn't spin on a broken critic.
    return { pass: true, notes: [], confidence: "medium" };
  }
}

// ----------------------------------------------------------------------------
// Finalization
// ----------------------------------------------------------------------------

// Extracts a tool-call summary list from the job's thought trail.
// Mirrors how the activity panel renders tool events: we want each
// entry to be `<name>(<arg-summary>)` so grader regexes can match on
// either name or args without parsing nested JSON.
function collectToolCallsFromThoughts(thoughts: Thought[]): string[] {
  const out: string[] = [];
  for (const t of thoughts) {
    if (t.kind !== "tool") continue;
    const text = (t.text ?? "").trim();
    const dataJson = t.data ? JSON.stringify(t.data) : "";
    // Compose `<text> <dataSnippet>` so a regex like /gmail/i matches
    // either the tool name or its argument summary.
    out.push(`${text} ${dataJson}`.trim().slice(0, 400));
  }
  return out;
}

// Best-effort eval recording. Wrapped in try/catch and fire-and-forget
// because eval logging must never block or fail a real /job. The
// recorder itself handles its own errors and logs a synthetic
// "error" run when it can't write — see recorder.ts.
async function recordJobEvalSafe(args: {
  jobId: string;
  meta: JobMeta | null;
  resultText?: string;
  artifacts?: string[];
  errorMessage?: string;
}): Promise<void> {
  try {
    const thoughts = await getThoughts(args.jobId, { limit: 400 });
    const toolCalls = collectToolCallsFromThoughts(thoughts);
    const startedAt = args.meta?.startedAt ?? args.meta?.createdAt ?? Date.now();
    const finishedAt = args.meta?.finishedAt ?? Date.now();
    await recordJobEval({
      jobId: args.jobId,
      input: {
        goal: args.meta?.prompt ?? "",
        modality: (args.meta as any)?.modality,
        channel: args.meta?.channel,
      },
      actual: {
        finalText: args.resultText ?? args.meta?.resultText,
        toolCalls,
        artifactPaths: args.artifacts ?? args.meta?.resultArtifacts ?? [],
        costUsd: args.meta?.estimatedCost,
        durationMs: Math.max(0, finishedAt - startedAt),
        jobStatus: args.meta?.status,
        errorMessage: args.errorMessage ?? args.meta?.error,
      },
    });
  } catch {
    // recorder is best-effort; never break finalize/fail
  }
}

// NOTE: there is deliberately NO hardcoded "which toolkit does this goal need"
// pre-flight gate here. Guessing a job's intended integration from regexes over
// the prompt is brittle and wrong (it can't cover every app, misfires on
// incidental mentions, and silently omits ones like Google Docs). Connection
// health is handled at RUNTIME by the executor's own tools instead: the agent
// can call check_integration_connected, and any live Composio call against an
// inactive connection returns a real error the agent surfaces (buildJobPrompt
// tells it to stop and report rather than fabricate success). That is
// intent-driven by the actual work, not by a hardcoded pattern table.

export async function finalizeJobStep(args: {
  jobId: string;
  tenantId: string;
  sessionId: string;
  channel: Channel;
  resultText: string;
  artifacts: string[];
  alreadyDelivered: boolean;
}): Promise<void> {
  "use step";

  await updateJobMeta(args.jobId, {
    status: "done",
    finishedAt: Date.now(),
    resultText: args.resultText,
    resultArtifacts: args.artifacts,
  });

  // Best-effort delivery. Outbound channel errors (Telegram chat not found,
  // SMS provider hiccup) must NOT mark the job as failed — the result is
  // already in Redis and the user can /status or /ask to read it. If we
  // didn't swallow this, WDK would retry the whole step (and the whole
  // pipeline if the step gets stuck), causing duplicate "job done" thoughts
  // and potentially duplicate user-facing messages.
  if (!args.alreadyDelivered && args.resultText) {
    try {
      await sendOutboundRuntime({
        channel: args.channel,
        sessionId: args.sessionId,
        text: args.resultText,
      });
    } catch (err: any) {
      await appendThought(args.jobId, {
        kind: "error",
        text: `final delivery failed: ${err?.message ?? String(err)}`,
      });
    }
  }

  await appendThought(args.jobId, {
    kind: "result",
    text: "job done",
  });

  // Mirror to per-tenant audit log for /ui Activity panel visibility.
  // Best-effort: audit failure should never mask a successful finalize.
  let metaForEval: JobMeta | null = null;
  try {
    metaForEval = await _getJobMeta(args.jobId);
    await recordAudit(metaForEval?.tenantId ?? args.tenantId, {
      kind: "tool.job_done",
      summary: `/job ${args.jobId} done`,
      meta: {
        jobId: args.jobId,
        kind: metaForEval?.kind,
        artifactCount: args.artifacts.length,
        durationMs: metaForEval?.startedAt
          ? Date.now() - metaForEval.startedAt
          : undefined,
      },
    });
  } catch {
    // best-effort
  }

  // Eval recording: a successful /job is still a data point worth
  // grading (did it call the right tools? was it fast enough? did it
  // produce artifacts?). Auto-logged + matched against any registered
  // seed cases. Failures here are swallowed inside recordJobEvalSafe.
  await recordJobEvalSafe({
    jobId: args.jobId,
    meta: metaForEval,
    resultText: args.resultText,
    artifacts: args.artifacts,
  });

  // Procedural memory: remember HOW this job was solved so a future similar
  // task can reuse the approach (Hermes-style "never forgets how it solved a
  // problem"). Deterministic + best-effort — one embedding, zero extra LLM
  // tokens, and a no-op for empty/failed results.
  if (args.resultText?.trim()) {
    try {
      const task = metaForEval?.prompt ?? "";
      if (task.trim()) {
        await recordSolution({
          tenantId: metaForEval?.tenantId ?? args.tenantId,
          meta: {
            source: "job",
            task,
            outcome: args.resultText,
            artifacts: args.artifacts,
            tags: metaForEval?.kind ? [metaForEval.kind] : [],
            ref: args.jobId,
          },
        });
      }
    } catch {
      // memory formation must never fail a finalized job
    }
  }
}

export async function failJobStep(args: {
  jobId: string;
  error: string;
}): Promise<void> {
  "use step";
  await updateJobMeta(args.jobId, {
    status: "failed",
    finishedAt: Date.now(),
    error: args.error,
  });
  await appendThought(args.jobId, {
    kind: "error",
    text: args.error,
  });

  let metaForEval: JobMeta | null = null;
  try {
    metaForEval = await _getJobMeta(args.jobId);
    if (metaForEval?.tenantId) {
      await recordAudit(metaForEval.tenantId, {
        kind: "tool.job_failed",
        summary: `/job ${args.jobId} failed: ${(args.error ?? "unknown").slice(0, 120)}`,
        meta: {
          jobId: args.jobId,
          kind: metaForEval.kind,
          error: args.error,
        },
      });
    }
  } catch {
    // best-effort
  }

  // Eval recording for failed jobs is the more important half — these
  // are the runs we most want to learn from. The grader's verdict on a
  // failure tells us *which* class of failure it was (retry exhaustion,
  // wrong tool, too slow, etc.) so future-me can spot patterns.
  await recordJobEvalSafe({
    jobId: args.jobId,
    meta: metaForEval,
    errorMessage: args.error,
  });
}

// ----------------------------------------------------------------------------
// Depth-reviewer state (escalation + depth-pass counter), persisted in jobMeta
// so it survives workflow resumptions. Read/written as durable steps.
// ----------------------------------------------------------------------------

export async function isJobEscalatedStep(jobId: string): Promise<boolean> {
  "use step";
  const meta = await _getJobMeta(jobId);
  return Boolean(meta?.escalated);
}

export async function getJobDepthStateStep(
  jobId: string
): Promise<{ escalated: boolean; depthPasses: number; maxDepthPasses: number }> {
  "use step";
  const meta = await _getJobMeta(jobId);
  const rawMax = Number(env("DEEP_MAX_DEPTH_PASSES") ?? "3");
  const maxDepthPasses = Number.isFinite(rawMax) && rawMax > 0 ? rawMax : 3;
  return {
    escalated: Boolean(meta?.escalated),
    depthPasses: meta?.depthPasses ?? 0,
    maxDepthPasses,
  };
}

export async function setJobEscalatedStep(args: {
  jobId: string;
  escalated: boolean;
}): Promise<void> {
  "use step";
  await updateJobMeta(args.jobId, { escalated: args.escalated });
}

export async function bumpJobDepthPassStep(args: {
  jobId: string;
}): Promise<void> {
  "use step";
  const meta = await _getJobMeta(args.jobId);
  await updateJobMeta(args.jobId, { depthPasses: (meta?.depthPasses ?? 0) + 1 });
}
