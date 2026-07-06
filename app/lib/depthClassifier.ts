// app/lib/depthClassifier.ts
//
// Decide whether a job should run in "deep / pro-extended" mode (orchestrator
// loop, reasoning-pro model, web search, longer wall-clock budget) or normal
// mode (single clarify-plan-execute-verify pass).
//
// Three-stage classification:
//   1. Explicit override:    /deep or /extended command → deep = true
//                            (handled by the route, not here)
//   2. Cheap heuristics:     length, keywords, modality → confident decision
//   3. LLM tiebreaker:       gpt-4.1 short JSON call for ambiguous cases
//
// The LLM tiebreaker only runs when heuristics are uncertain — keeps the
// median dispatch latency low while still catching nuance.

import { generateObject } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
// We can't record cost here because depth classification happens BEFORE the
// job is created (no jobId yet). The classifier call is small (<1k tokens)
// and runs once per dispatch, so the leak is bounded.

export type DepthDecision = {
  deep: boolean;
  source: "explicit" | "heuristic" | "llm";
  reason: string;
};

// Strongly-deep keywords — presence alone tips the scale.
const DEEP_KEYWORDS: RegExp[] = [
  /\bdeep[\s-]?research\b/i,
  /\bdeep[\s-]?dive\b/i,
  /\bcomprehensive\b/i,
  /\bextensive\b/i,
  /\bexhaustive\b/i,
  /\bend[\s-]?to[\s-]?end\b/i,
  /\bcomplete\s+(framework|system|implementation|design|spec)/i,
  /\bfrom\s+scratch\b/i,
  /\bproduction[\s-]?grade\b/i,
  /\bfull\s+(spec|design|implementation|build)/i,
  /\bfull[\s-]?stack\b/i,
  /\bzk[\s-]?(snark|stark)\b/i,
  /\bcircuit\b/i,
  /\b\d{2,3}\s*[\s-]*(page|pp)s?\b/i, // "50 pages", "100 pp"
  /\bwhite[\s-]?paper\b/i,
  /\bsurvey\b/i,
  /\bbenchmark/i,
  /\barchitect(ure|ing)\b/i,
];

// Anti-deep keywords — presence pulls toward shallow.
const SHALLOW_KEYWORDS: RegExp[] = [
  /^\s*(hi|hello|hey|sup|yo|thanks|thank you|ok|okay|cool|nice)\s*[!.?]?\s*$/i,
  /\bquick\b/i,
  /\bbriefly?\b/i,
  /\bone[\s-]?(line|sentence|paragraph)\b/i,
  /\btl;?dr\b/i,
  /\bjust\s+/i,
];

const DEEP_WORD_THRESHOLD = 60; // > 60 words → leans deep
const SHALLOW_WORD_THRESHOLD = 12; // < 12 words → leans shallow

function wordCount(text: string): number {
  return (text.trim().match(/\S+/g) ?? []).length;
}

function matches(patterns: RegExp[], text: string): number {
  return patterns.reduce((n, re) => (re.test(text) ? n + 1 : n), 0);
}

// Returns:
//   { deep: true|false, confident: true }   → committed answer
//   { confident: false }                     → defer to LLM tiebreaker
function heuristicClassify(
  prompt: string
):
  | { deep: boolean; confident: true; reason: string }
  | { confident: false; reason: string } {
  const text = prompt ?? "";
  const wc = wordCount(text);
  const deepHits = matches(DEEP_KEYWORDS, text);
  const shallowHits = matches(SHALLOW_KEYWORDS, text);

  if (deepHits >= 2) {
    return {
      deep: true,
      confident: true,
      reason: `heuristic: ${deepHits} deep-keyword hits`,
    };
  }
  if (shallowHits >= 1 && wc <= SHALLOW_WORD_THRESHOLD) {
    return {
      deep: false,
      confident: true,
      reason: `heuristic: ${shallowHits} shallow-keyword hit + ${wc} words`,
    };
  }
  if (deepHits >= 1 && wc >= DEEP_WORD_THRESHOLD) {
    return {
      deep: true,
      confident: true,
      reason: `heuristic: 1 deep-keyword hit + ${wc} words`,
    };
  }
  if (wc < SHALLOW_WORD_THRESHOLD && deepHits === 0) {
    return {
      deep: false,
      confident: true,
      reason: `heuristic: ${wc} words, no deep markers`,
    };
  }
  if (wc > DEEP_WORD_THRESHOLD * 2) {
    return {
      deep: true,
      confident: true,
      reason: `heuristic: very long prompt (${wc} words)`,
    };
  }
  return {
    confident: false,
    reason: `heuristic uncertain (wc=${wc}, deepHits=${deepHits}, shallowHits=${shallowHits})`,
  };
}

const llmClassifySchema = z.object({
  deep: z.boolean(),
  reason: z.string(),
});

async function llmClassify(prompt: string): Promise<DepthDecision> {
  const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0.1 });

  const system = [
    "Classify whether a user request should run in 'deep / pro-extended' mode",
    "or normal mode for an autonomous agent.",
    "",
    "DEEP mode is justified when the task:",
    "  - requires multi-step research / synthesis",
    "  - asks for a full implementation, framework, paper, or 50+ page artifact",
    "  - explicitly demands depth, completeness, or production quality",
    "  - is highly technical (ZK circuits, distributed systems, etc.)",
    "  - cannot be reasonably answered by a single 30-second LLM call",
    "",
    "NORMAL mode is for:",
    "  - quick questions",
    "  - short one-shot generations (paragraph, snippet, summary)",
    "  - casual chat",
    "  - anything that benefits little from a 30+ minute reasoning loop",
    "",
    "Output:",
    "  deep:   true/false",
    "  reason: one-sentence justification",
  ].join("\n");

  try {
    const { object } = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: llmClassifySchema,
      system,
      prompt: `User request:\n${prompt}`,
    });
    return { deep: object.deep, source: "llm", reason: object.reason };
  } catch (err: any) {
    // On classifier failure, default to NORMAL mode. Deep mode is expensive;
    // failing to shallow keeps cost predictable.
    return {
      deep: false,
      source: "llm",
      reason: `llm classifier failed (${err?.message ?? String(err)}); defaulting to normal`,
    };
  }
}

export async function classifyDepth(
  prompt: string,
  opts?: { explicit?: boolean }
): Promise<DepthDecision> {
  if (opts?.explicit) {
    return { deep: true, source: "explicit", reason: "user used /deep or /extended" };
  }

  const heuristic = heuristicClassify(prompt);
  if (heuristic.confident) {
    return { deep: heuristic.deep, source: "heuristic", reason: heuristic.reason };
  }

  return llmClassify(prompt);
}
