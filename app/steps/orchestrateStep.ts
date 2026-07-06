// app/steps/orchestrateStep.ts
//
// The orchestrator is the brain of pro-extended deep mode. Each iteration it
// reads (goal, assumptions, accumulated subtask outputs) and decides the
// SINGLE next action: do more research, execute a subtask, or finalize.
// Sequential by default; the LLM may declare a batch as parallel when it
// chooses (slice 4 will actually fan out in parallel — slice 3 falls back
// to sequential execution of the batch).
//
// Model: reasoning-pro (o3-pro by default) for maximum planning depth.
// Falls back through the chain in modelRouting if the configured pro model
// is unavailable.

import { generateObject } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import { appendThought } from "@/app/lib/jobStore";
import {
  type SubtaskResult,
} from "@/app/machines/jobMachine";
import type { ModalityId } from "@/app/lib/rubrics";
import { recordCost, deepBudgetUsd, deepEscalatedBudgetUsd } from "@/app/lib/costTracker";
import { env } from "@/app/lib/env";
import { maybeUpgradePurpose } from "@/app/lib/learn/routerBias";

const MODALITY_IDS = [
  "code-rust",
  "code-rust-zk",
  "code-ui-nextjs-ts",
  "code-generic",
  "latex-pdf",
  "research",
  "generic",
] as const satisfies readonly ModalityId[];

const subtaskKindEnum = z.enum(["research", "execute", "synthesize"]);

const orchestrateSchema = z.object({
  // What to do next. "done" means the orchestrator is satisfied; the
  // finalSynthesis field below MUST be populated.
  //
  //   research   — web search subagent (gpt-4.1 + native browsing)
  //   execute    — run agentTurn with a concrete instruction (general work)
  //   synthesize — combine prior subtask outputs into a writeup
  //   compute    — hand to OpenAI code_interpreter for Python-grade numeric
  //                work, parsing, plotting, simulation. The orchestrator
  //                picks this when a problem has a deterministic compute
  //                core that benefits from real code execution.
  //   done       — orchestrator is satisfied; finalSynthesis populated
  action: z.enum(["research", "execute", "synthesize", "compute", "done"]),
  reasoning: z.string(),
  goal: z.string(),
  instructions: z.string(),
  query: z.string().nullable(),
  finalSynthesis: z.string().nullable(),
  modality: z.enum(MODALITY_IDS).nullable(),
  // Optional fan-out: when the next step is several INDEPENDENT subtasks that
  // can run at once (no data dependency between them), the orchestrator may
  // return up to 4 here. The workflow runs them in parallel as nested
  // sub-agents in a single iteration. Leave empty for sequential work.
  parallelSubtasks: z
    .array(
      z.object({
        action: z.enum(["research", "execute", "synthesize", "compute"]),
        goal: z.string(),
        instructions: z.string(),
        query: z.string().nullable(),
        modality: z.enum(MODALITY_IDS).nullable(),
      })
    )
    .max(4)
    .nullable(),
});

export type ParallelSubtaskSpec = {
  action: "research" | "execute" | "synthesize" | "compute";
  goal: string;
  instructions: string;
  query: string | null;
  modality: ModalityId | null;
};

export type OrchestrateDecision = {
  action: "research" | "execute" | "synthesize" | "compute" | "done";
  reasoning: string;
  goal: string;
  instructions: string;
  query: string | null;
  finalSynthesis: string | null;
  modality: ModalityId | null;
  parallelSubtasks: ParallelSubtaskSpec[] | null;
};

// Hard cap to keep the orchestrator from infinite-looping. The dollar budget
// (deepBudgetUsd, default $5) is the primary cost ceiling; this is just a
// last-line safety bound. Bumped 14→22 so deep jobs can think more,
// retry sub-strategies, and run more orchestrator turns when the budget
// allows.
export const MAX_ORCHESTRATOR_ITERATIONS = Number(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).process?.env?.DEEP_MAX_ITERATIONS ?? "28"
) || 28;

// How many orchestrator steps a single attempt may take before it MUST draft
// a final answer (so the verifier + depth reviewer get to run). Module-level
// so it's read in normal context, not the workflow VM.
export const DEEP_PER_ATTEMPT_ITERS = Math.max(
  3,
  Number(
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).process?.env?.DEEP_PER_ATTEMPT_ITERS ?? "7"
  ) || 7
);

export async function orchestrateStep(args: {
  jobId: string;
  prompt: string;
  assumptions: string[];
  subtaskResults: SubtaskResult[];
  verifierNotes: string[];
  iter: number;
  costUsd: number;
  // Set by the depth reviewer when this job has been escalated to the top
  // model tier. Forces gpt-5.4-pro (smart-pro) at highest reasoning effort.
  escalated?: boolean;
  // When true, the orchestrator MUST finalize this turn (action="done") —
  // the workflow sets it once the current attempt has used its per-attempt
  // iteration budget, so the verifier + depth reviewer get a fresh draft to
  // evaluate instead of the orchestrator researching indefinitely.
  forceFinalize?: boolean;
}): Promise<OrchestrateDecision> {
  "use step";

  // Tiered orchestrator routing:
  //
  //   default        — `meta` (gpt-5.4). Real thinking quality with much
  //                    better planning than the deprecated gpt-5.2; the
  //                    extra cost is worth it for multi-step research /
  //                    compute work.
  //   near-end       — `meta-pro` (gpt-5.3-codex). Sharper synthesis on
  //                    the last 2 iterations so the final answer is
  //                    well-formed; codex's longer reasoning traces
  //                    handle the structured-output requirements better
  //                    than gpt-5.2-pro did.
  //   revise pass    — `meta-pro`. When the verifier rejected a prior
  //                    attempt, the next plan needs to actually fix the
  //                    notes — pay for higher-quality reasoning.
  //   DEEP_USE_REASONING_PRO=true — forces o3-pro, the absolute top tier.
  //                    Expensive; only use when budget allows.
  const cap = args.escalated ? deepEscalatedBudgetUsd() : deepBudgetUsd();
  const inReviseMode = args.verifierNotes.length > 0;
  const nearEnd = args.iter >= MAX_ORCHESTRATOR_ITERATIONS - 2;
  const forcePro = env("DEEP_USE_REASONING_PRO") === "true";

  // Escalated jobs run the orchestrator on gpt-5.4-pro (smart-pro) at the
  // highest reasoning effort — the depth reviewer decided the answer needs a
  // genuine step up in reasoning power, not just another pass. A forced
  // finalize also bumps to the pro synthesis tier so the draft is well-formed.
  const purpose:
    | "meta"
    | "meta-pro"
    | "smart-pro"
    | "reasoning-pro" = forcePro
    ? "reasoning-pro"
    : args.escalated
      ? "smart-pro"
      : inReviseMode || nearEnd || args.forceFinalize
        ? "meta-pro"
        : await maybeUpgradePurpose(
            "meta",
            "meta-pro",
            "orchestrator-default",
            `iter:${args.iter < 2 ? "early" : "mid"}`,
            args.jobId
          ).catch(() => "meta" as const);

  const llm = buildLlmArgs({ purpose, temperature: 0.4 });
  // Force highest reasoning effort when escalated, regardless of the global
  // REASONING_EFFORT default.
  if (args.escalated && llm.providerOptions?.openai) {
    llm.providerOptions.openai.reasoningEffort = "high";
  }

  await appendThought(args.jobId, {
    kind: "info",
    text: `orchestrator iter ${args.iter + 1}: model=${llm.modelName} purpose=${purpose}${inReviseMode ? " (revise)" : ""}${nearEnd ? " (near-end)" : ""}`,
    data: { iter: args.iter + 1, purpose, model: llm.modelName },
  });

  const subtaskSummary = args.subtaskResults
    .map((s, i) => {
      const out = (s.output ?? "").slice(0, 1000);
      const cite =
        s.citations.length > 0
          ? `\n  citations: ${s.citations.slice(0, 6).join(", ")}`
          : "";
      const arts =
        s.artifacts.length > 0
          ? `\n  artifacts: ${s.artifacts.slice(0, 6).join(", ")}`
          : "";
      return `[${i + 1}] (${s.kind}) ${s.goal}\n  output: ${out}${cite}${arts}`;
    })
    .join("\n\n");

  const system = [
    "You are the orchestrator for an autonomous agent running in pro-extended",
    "deep mode. The agent is solving a high-stakes user request that may take",
    "30-60 minutes of compute time and require multiple research and execution",
    "subtasks.",
    "",
    "Your role each turn: read the goal, the user's assumptions, and the",
    "outputs of all completed subtasks so far. Decide what the agent should",
    "do NEXT. Choose ONE action:",
    "",
    "  research    — run a web search subagent (gpt-4.1 + native browsing)",
    "                for a specific query. Use when you need facts,",
    "                citations, or context from the open web.",
    "  execute     — run the executor agent with a concrete instruction.",
    "                Use for code, prose, tables, structured writeups.",
    "  synthesize  — combine prior subtask outputs into a longer intermediate",
    "                writeup. Use when you have raw research and need to",
    "                integrate it before the final answer.",
    "  compute     — hand the task to OpenAI's code_interpreter in a hosted",
    "                Python container. Use when the next step has a",
    "                deterministic numeric / parsing / data-wrangling /",
    "                plot-generation core that benefits from real code",
    "                execution (NOT just describing what code would do).",
    "                Pick this for: symbolic math, eigenvalues, file",
    "                parsing, statistical analysis, chart rendering,",
    "                deterministic simulations.",
    "  done        — you are satisfied. Provide the COMPLETE final answer in",
    "                finalSynthesis. This goes directly to the user after",
    "                verification.",
    "",
    "PARALLEL FAN-OUT: when the next move is several INDEPENDENT subtasks with",
    "no data dependency between them (e.g. research three different sources at",
    "once, or compute two unrelated quantities), return them as a batch of up",
    "to 4 in `parallelSubtasks` (each with its own action/goal/instructions).",
    "They run simultaneously as nested sub-agents in ONE iteration — much",
    "faster than sequential. Only batch genuinely independent work; if subtask",
    "B needs subtask A's output, do them sequentially (leave parallelSubtasks",
    "empty and pick a single action). Still set the top-level action/goal as a",
    "representative of the batch.",
    "",
    "Rules:",
    "1. Plan ahead. Don't pick 'done' until the answer is truly complete and",
    "   meets the standard the user implied. Half-baked output will be",
    "   rejected by the critic and you'll have to redo work.",
    "2. Think more before acting — your prior subtask outputs are below; read",
    "   them carefully before deciding what's still missing.",
    "3. Prefer compute over execute when the next step is genuinely about",
    "   running code. Real execution beats describing code that wasn't run.",
    "4. Don't over-research. Stop searching once you have what you need.",
    "5. Each subtask should make concrete progress; no busywork.",
    "6. If a verifier note is present, the prior attempt failed — your next",
    "   action must address the specific feedback before doing anything else.",
    "7. When you pick 'done', the modality field MUST be set to the right",
    "   category so the critic uses the correct rubric.",
    "8. When you pick 'done', the `finalSynthesis` lands directly in the",
    "   user's chat (Telegram). Write it like you're texting a smart friend",
    "   the conclusion of their question — open with the actual answer, then",
    "   the supporting detail. Avoid 'In this report…' / 'Executive summary…'",
    "   intros. Use markdown headers / lists only if the answer really wants",
    "   them (a list of steps, a comparison table). Casual contractions are",
    "   fine. Cite sources inline as bare URLs where they came up.",
    "9. GROUNDING: every factual claim in `finalSynthesis` must trace to a",
    "   subtask output below (or the user's own prompt). Do not add facts,",
    "   numbers, links, or 'we did X' claims from memory. If a subtask FAILED,",
    "   say what's missing because of it — never paper over the gap with",
    "   invented content.",
    "",
    `Iteration ${args.iter + 1} of at most ${MAX_ORCHESTRATOR_ITERATIONS}.`,
    `Cost so far: $${args.costUsd.toFixed(3)} / $${cap.toFixed(2)} budget` +
      ` (${Math.round((args.costUsd / cap) * 100)}% used).`,
    args.iter >= MAX_ORCHESTRATOR_ITERATIONS - 2
      ? "WARN: nearing iteration cap — finalize next turn unless absolutely necessary."
      : "",
    args.costUsd / cap >= 0.7
      ? "WARN: >70% of budget consumed — be decisive, don't add more research/synthesis/compute unless critical."
      : "",
    args.costUsd / cap >= 0.9
      ? "URGENT: >90% of budget consumed — your next action MUST be 'done' with the best synthesis you can produce from what's been gathered."
      : "",
    args.forceFinalize
      ? "FINALIZE NOW: this attempt has used its iteration budget. Your action MUST be 'done'. Write the most complete, insightful, data-rich finalSynthesis you can from everything gathered so far. A reviewer will then decide if more passes are warranted — your job right now is to produce the best possible draft, not to gather more."
      : "",
  ]
    .filter(Boolean)
    .join("\n");

  const userPrompt = [
    `User goal:\n${args.prompt}`,
    args.assumptions.length
      ? `\nAssumptions:\n- ${args.assumptions.join("\n- ")}`
      : "",
    args.verifierNotes.length
      ? `\nVerifier notes from prior attempt — MUST FIX:\n- ${args.verifierNotes.join("\n- ")}`
      : "",
    args.subtaskResults.length
      ? `\nCompleted subtasks (${args.subtaskResults.length}):\n${subtaskSummary}`
      : "\nNo subtasks completed yet.",
  ]
    .filter(Boolean)
    .join("\n");

  try {
    const result = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: orchestrateSchema,
      system,
      prompt: userPrompt,
      // A hung provider call otherwise runs until the platform kills the step
      // at maxDuration (uncatchable) → WDK retries the same hang → "exceeded
      // max retries" after ~5×800s. Abort below the kill line so the catch
      // path can force a graceful "done" instead.
      abortSignal: AbortSignal.timeout(
        Number(env("DEEP_ORCHESTRATE_TIMEOUT_MS") ?? "420000") || 420000
      ),
    });
    const { object } = result;
    await recordCost({
      jobId: args.jobId,
      model: llm.modelName,
      usage: (result as any).usage,
    });

    await appendThought(args.jobId, {
      kind: "reasoning",
      text: `orchestrator iter ${args.iter + 1} [${llm.modelName}]: ${object.action} — ${object.reasoning.slice(0, 200)}`,
      data: {
        iter: args.iter + 1,
        model: llm.modelName,
        action: object.action,
        goal: object.goal,
        modality: object.modality,
        costUsd: args.costUsd,
      },
    });

    // Coerce to "done" when finalize was forced but the model still tried to
    // gather more — use whatever synthesis/instructions it produced, falling
    // back to the last subtask output so we always ship a draft.
    if (args.forceFinalize && object.action !== "done") {
      const fallback =
        object.finalSynthesis ||
        object.instructions ||
        args.subtaskResults[args.subtaskResults.length - 1]?.output ||
        "";
      await appendThought(args.jobId, {
        kind: "info",
        text: `forced finalize: coerced action ${object.action} → done`,
      });
      return {
        action: "done",
        reasoning: object.reasoning,
        goal: object.goal,
        instructions: object.instructions,
        query: object.query,
        finalSynthesis: fallback,
        modality: object.modality ?? "generic",
        parallelSubtasks: null,
      };
    }

    return {
      action: object.action,
      reasoning: object.reasoning,
      goal: object.goal,
      instructions: object.instructions,
      query: object.query,
      finalSynthesis: object.finalSynthesis,
      modality: object.modality,
      parallelSubtasks:
        object.parallelSubtasks && object.parallelSubtasks.length
          ? (object.parallelSubtasks as ParallelSubtaskSpec[])
          : null,
    };
  } catch (err: any) {
    // Orchestrator failure is bad — there's no obvious fallback at this
    // tier. Log and emit a "done" decision with whatever the last subtask
    // produced, so the job ships something rather than spinning.
    await appendThought(args.jobId, {
      kind: "error",
      text: `orchestrator failed (forcing done): ${err?.message ?? String(err)}`,
    });
    const lastOutput =
      args.subtaskResults[args.subtaskResults.length - 1]?.output ?? "";
    return {
      action: "done",
      reasoning: "orchestrator failure — finalizing with last subtask output",
      goal: "",
      instructions: "",
      query: null,
      finalSynthesis:
        lastOutput ||
        "Sorry — the orchestrator failed to produce a final answer. Please retry.",
      modality: "generic",
      parallelSubtasks: null,
    };
  }
}
