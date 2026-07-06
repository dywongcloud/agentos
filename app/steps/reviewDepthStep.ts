// app/steps/reviewDepthStep.ts
//
// The DEPTH reviewer sub-agent for deep jobs. The existing verifyStep is a
// correctness critic ("does this satisfy the rubric / is it complete?"). This
// reviewer asks a different, harder question: "is this as INSIGHTFUL and
// DATA-RICH as it should be, or can the agent dig deeper?"
//
// It scores the draft on four axes and returns a verdict:
//   accept       — the answer is genuinely deep; ship it.
//   more_passes  — solid but shallow in places; do more research/compute
//                  passes to fill the named gaps (same model tier).
//   escalate     — needs a real step up in reasoning; bump the orchestrator
//                  and subtasks to gpt-5.4-pro (highest effort) and re-attack
//                  the gaps. Only escalates once.
//
// Two heads, on purpose:
//   1. A strong OpenAI reasoning critic (o3 by default) does the scoring +
//      verdict.
//   2. Gemini 3.1 Pro gives a cross-family second opinion — "what specific
//      data points, counterarguments, or angles is this missing?" — whose
//      suggestions are merged into the gaps the next pass must address.
//
// Everything is best-effort: any failure degrades to "accept" so a flaky
// reviewer can never trap a job in an infinite loop or block delivery.

import { generateObject, generateText } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs, resolveModel, resolveModelName } from "@/app/lib/modelRouting";
import { appendThought } from "@/app/lib/jobStore";
import { recordCost } from "@/app/lib/costTracker";
import { env } from "@/app/lib/env";
import type { SubtaskResult } from "@/app/machines/jobMachine";

export type DepthVerdict = "accept" | "more_passes" | "escalate";

export type ReviewDepthResult = {
  verdict: DepthVerdict;
  scores: {
    insight: number;
    dataDensity: number;
    coverage: number;
    rigor: number;
  };
  avg: number;
  gaps: string[]; // concrete, actionable: what to add in the next pass
  geminiNotes: string[]; // cross-family suggestions (subset of gaps)
};

const reviewSchema = z.object({
  insight: z.number().min(1).max(10).describe("Depth of analysis & non-obvious insight (1-10)."),
  data_density: z.number().min(1).max(10).describe("Concrete data points, numbers, citations, specifics (1-10)."),
  coverage: z.number().min(1).max(10).describe("Breadth — are all important angles covered? (1-10)."),
  rigor: z.number().min(1).max(10).describe("Logical soundness, sourcing, lack of hand-waving (1-10)."),
  gaps: z
    .array(z.string())
    .max(10)
    .describe("Specific, actionable gaps — what concrete data/analysis/angle to add next. Empty if none."),
  verdict_reason: z.string().describe("One-sentence rationale for the scores."),
});

function acceptThreshold(): number {
  const n = Number(env("DEEP_REVIEW_ACCEPT_SCORE") ?? "8");
  return Number.isFinite(n) ? n : 8;
}
function escalateThreshold(): number {
  const n = Number(env("DEEP_REVIEW_ESCALATE_SCORE") ?? "6.5");
  return Number.isFinite(n) ? n : 6.5;
}

// Gemini model used for the cross-family second opinion. Reuses the same
// Gemini 3.1 Pro the browser side-car uses; override via DEEP_GEMINI_MODEL.
function geminiReviewModel(): string {
  return env("DEEP_GEMINI_MODEL") ?? resolveModelName("browser-pro");
}
function geminiAvailable(): boolean {
  const m = geminiReviewModel().toLowerCase();
  if (m.startsWith("gemini") || m.startsWith("google/")) {
    return !!env("GOOGLE_GENERATIVE_AI_API_KEY") || !!env("GOOGLE_API_KEY");
  }
  return true;
}

export async function reviewDepthStep(args: {
  jobId: string;
  prompt: string;
  draft: string;
  subtaskResults: SubtaskResult[];
  alreadyEscalated: boolean;
  depthPass: number;
  costUsd: number;
  budgetCap: number;
}): Promise<ReviewDepthResult> {
  "use step";

  const subtaskDigest = args.subtaskResults
    .map((s, i) => `[${i + 1}] (${s.kind}) ${s.goal}: ${(s.output ?? "").slice(0, 400)}`)
    .join("\n");

  // --- Head 1: OpenAI reasoning critic (scores + gaps) ----------------------
  // Use the top reasoning tier — this judgment call is worth the spend, and it
  // runs at most a handful of times per job.
  const llm = buildLlmArgs({ purpose: "reasoning", temperature: 0.2 });

  const system = [
    "You are a demanding research editor reviewing a draft answer to a",
    "high-stakes request. The draft already passed a correctness check — your",
    "job is to judge DEPTH, not correctness.",
    "",
    "Score 1-10 on four axes:",
    "  insight      — does it surface non-obvious connections, second-order",
    "                 effects, tradeoffs? Or is it surface-level summary?",
    "  data_density — concrete numbers, dates, named examples, citations,",
    "                 quantified claims? Or vague generalities?",
    "  coverage     — are the important angles/sub-questions addressed?",
    "  rigor        — sound reasoning, sourced claims, no hand-waving?",
    "",
    "Be a tough grader. An 8+ means genuinely excellent and hard to improve.",
    "A 6 means competent but clearly leaves depth on the table. List SPECIFIC,",
    "actionable gaps — name the exact data point, comparison, or angle to add.",
    "Don't pad gaps; if it's genuinely excellent, return an empty gaps list.",
  ].join("\n");

  const userPrompt = [
    `Original request:\n${args.prompt}`,
    "",
    `Draft answer:\n${args.draft.slice(0, 12000)}`,
    args.subtaskResults.length
      ? `\nWork done so far (for context on what's already been gathered):\n${subtaskDigest.slice(0, 6000)}`
      : "",
  ]
    .filter(Boolean)
    .join("\n");

  let scores = { insight: 8, dataDensity: 8, coverage: 8, rigor: 8 };
  let gaps: string[] = [];
  try {
    const result = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: reviewSchema,
      system,
      prompt: userPrompt,
    });
    const o = result.object;
    scores = {
      insight: o.insight,
      dataDensity: o.data_density,
      coverage: o.coverage,
      rigor: o.rigor,
    };
    gaps = o.gaps ?? [];
    await recordCost({ jobId: args.jobId, model: llm.modelName, usage: (result as any).usage });
  } catch (err: any) {
    await appendThought(args.jobId, {
      kind: "error",
      text: `depth-review critic failed (degrading to accept): ${err?.message ?? String(err)}`,
    });
    return {
      verdict: "accept",
      scores,
      avg: 8,
      gaps: [],
      geminiNotes: [],
    };
  }

  // --- Head 2: Gemini 3.1 Pro cross-family second opinion -------------------
  // A different model family catches different blind spots. We only ask for
  // additional concrete gaps; failures are non-fatal.
  let geminiNotes: string[] = [];
  const budgetLeft = args.budgetCap - args.costUsd;
  if (geminiAvailable() && budgetLeft > 0.5) {
    const gModel = geminiReviewModel();
    try {
      const g = await generateText({
        model: resolveModel(gModel),
        temperature: 0.5,
        system:
          "You're a sharp analyst giving a second opinion on a draft answer. " +
          "List up to 6 SPECIFIC things that would make it deeper or more " +
          "useful: a concrete data point to add, a counterargument to address, " +
          "an angle that's missing, a comparison worth making. One per line, " +
          "terse, no preamble, no numbering. If it's already excellent, reply NONE.",
        prompt: `Request: ${args.prompt}\n\nDraft:\n${args.draft.slice(0, 10000)}`,
      });
      const text = (g.text ?? "").trim();
      if (text && text.toUpperCase() !== "NONE") {
        geminiNotes = text
          .split("\n")
          .map((l) => l.replace(/^[-*\d.)\s]+/, "").trim())
          .filter((l) => l.length > 3)
          .slice(0, 6);
      }
      await recordCost({ jobId: args.jobId, model: gModel, usage: (g as any).usage });
    } catch (err: any) {
      await appendThought(args.jobId, {
        kind: "info",
        text: `gemini cross-review skipped: ${err?.message ?? String(err)}`,
      });
    }
  }

  // Merge gaps (dedup-ish) — Gemini's notes are prefixed so the next pass sees
  // the cross-family perspective explicitly.
  const mergedGaps = [
    ...gaps,
    ...geminiNotes.map((n) => `[cross-review] ${n}`),
  ];

  const avg = (scores.insight + scores.dataDensity + scores.coverage + scores.rigor) / 4;

  // --- Verdict --------------------------------------------------------------
  let verdict: DepthVerdict;
  if (avg >= acceptThreshold() && mergedGaps.length === 0) {
    verdict = "accept";
  } else if (avg >= acceptThreshold()) {
    // High score but the reviewers still named gaps — one more pass is cheap
    // insurance unless we're out of budget.
    verdict = budgetLeft > 1.0 ? "more_passes" : "accept";
  } else if (!args.alreadyEscalated && avg < escalateThreshold() && budgetLeft > 2.0) {
    verdict = "escalate";
  } else {
    verdict = budgetLeft > 1.0 ? "more_passes" : "accept";
  }

  await appendThought(args.jobId, {
    kind: "observation",
    text:
      `depth-review pass ${args.depthPass}: avg=${avg.toFixed(1)} ` +
      `(insight=${scores.insight} data=${scores.dataDensity} coverage=${scores.coverage} rigor=${scores.rigor}) ` +
      `→ ${verdict}${geminiNotes.length ? ` +${geminiNotes.length} gemini notes` : ""}`,
    data: { scores, avg, verdict, gapCount: mergedGaps.length },
  });

  return { verdict, scores, avg, gaps: mergedGaps, geminiNotes };
}
