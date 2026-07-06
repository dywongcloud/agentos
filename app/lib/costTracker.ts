// app/lib/costTracker.ts
//
// Per-job cost accounting. Every LLM call site records its `usage` (input +
// output tokens) and the model used. We multiply by a pricing table to get
// estimated USD. The accumulator lives at `job:{id}:cost` in Redis and is
// mirrored into the job meta's `estimatedCost` field so /status and the
// orchestrator can see it.
//
// The pricing table is best-effort — OpenAI's prices change. Values can be
// overridden per-model via env:
//   PRICE_<MODEL_KEY>_IN   USD per 1M input tokens
//   PRICE_<MODEL_KEY>_OUT  USD per 1M output tokens
// where MODEL_KEY is the model name with `.`, `-` replaced by `_` and
// uppercased (e.g. `gpt-5.4-mini` → `GPT_5_4_MINI`).

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import { updateJobMeta } from "@/app/lib/jobStore";

// Pricing in USD per 1M tokens. Numbers are reasonable estimates as of the
// model lineup the user picked; override via env if OpenAI publishes changes.
const DEFAULT_PRICING: Record<string, { in: number; out: number }> = {
  // Cheap workhorses
  "gpt-4o-mini": { in: 0.15, out: 0.6 },
  "gpt-5.4-mini": { in: 0.5, out: 2.0 },
  "gpt-4.1-mini": { in: 0.4, out: 1.6 },

  // Mid
  "gpt-4o": { in: 2.5, out: 10.0 },
  "gpt-4.1": { in: 2.0, out: 8.0 },
  "gpt-5.4": { in: 2.5, out: 20.0 },
  "gpt-5.3-codex": { in: 2.0, out: 10.0 },
  "gpt-5.2": { in: 1.25, out: 10.0 },

  // Premium
  "gpt-5.4-pro": { in: 15.0, out: 60.0 },
  "o3-mini": { in: 1.1, out: 4.4 },
  "o3": { in: 15.0, out: 60.0 },
  "o3-pro": { in: 50.0, out: 200.0 },
};

// Conservative fallback for unknown models — assume premium.
const UNKNOWN_PRICING = { in: 15.0, out: 60.0 };

function envKey(model: string): string {
  return "PRICE_" + model.replace(/[.\-]/g, "_").toUpperCase();
}

function pricingFor(model: string): { in: number; out: number } {
  const base = DEFAULT_PRICING[model] ?? UNKNOWN_PRICING;
  const envIn = env(envKey(model) + "_IN");
  const envOut = env(envKey(model) + "_OUT");
  return {
    in: envIn != null && Number.isFinite(Number(envIn)) ? Number(envIn) : base.in,
    out:
      envOut != null && Number.isFinite(Number(envOut))
        ? Number(envOut)
        : base.out,
  };
}

// AI SDK v5 surfaces usage on the resolved result. Different code paths use
// different field names (`inputTokens`/`outputTokens` for v5, `promptTokens`/
// `completionTokens` legacy). Normalize.
export type RawUsage = {
  inputTokens?: number | null;
  outputTokens?: number | null;
  totalTokens?: number | null;
  promptTokens?: number | null;
  completionTokens?: number | null;
  cached_tokens?: number | null;
  cachedInputTokens?: number | null;
} | null | undefined;

export type NormalizedUsage = {
  inputTokens: number;
  outputTokens: number;
};

export function normalizeUsage(u: RawUsage): NormalizedUsage {
  if (!u || typeof u !== "object") return { inputTokens: 0, outputTokens: 0 };
  const inputTokens =
    Number(u.inputTokens ?? u.promptTokens ?? 0) || 0;
  const outputTokens =
    Number(u.outputTokens ?? u.completionTokens ?? 0) || 0;
  return { inputTokens, outputTokens };
}

export function estimateUsd(model: string, usage: NormalizedUsage): number {
  const p = pricingFor(model);
  return (usage.inputTokens * p.in + usage.outputTokens * p.out) / 1_000_000;
}

// Rough estimate from text length when actual usage isn't available.
// ~4 chars per token is the OpenAI rule of thumb.
export function estimateUsageFromText(
  promptText: string,
  outputText: string
): NormalizedUsage {
  return {
    inputTokens: Math.max(0, Math.ceil((promptText?.length ?? 0) / 4)),
    outputTokens: Math.max(0, Math.ceil((outputText?.length ?? 0) / 4)),
  };
}

// --- per-job accumulator ----------------------------------------------------

export type JobCost = {
  usd: number;
  tokens: { in: number; out: number };
  byModel: Record<string, { in: number; out: number; usd: number; calls: number }>;
};

const EMPTY_COST: JobCost = {
  usd: 0,
  tokens: { in: 0, out: 0 },
  byModel: {},
};

function costKey(jobId: string): string {
  return `job:${jobId}:cost`;
}

export async function getJobCost(jobId: string): Promise<JobCost> {
  const store = getStore();
  const cur = await store.get<JobCost>(costKey(jobId));
  return cur ?? { ...EMPTY_COST, byModel: {} };
}

export async function recordCost(args: {
  jobId: string;
  model: string;
  usage: RawUsage;
  // For tools that don't return usage (e.g. some streaming returns), the
  // caller can provide raw text and we estimate.
  promptText?: string;
  outputText?: string;
}): Promise<{ usd: number; total: JobCost }> {
  const norm: NormalizedUsage =
    args.usage != null
      ? normalizeUsage(args.usage)
      : estimateUsageFromText(args.promptText ?? "", args.outputText ?? "");

  const usd = estimateUsd(args.model, norm);

  const store = getStore();
  const cur = (await store.get<JobCost>(costKey(args.jobId))) ?? {
    ...EMPTY_COST,
    byModel: {},
  };

  const byModel = cur.byModel ?? {};
  const prev = byModel[args.model] ?? { in: 0, out: 0, usd: 0, calls: 0 };
  byModel[args.model] = {
    in: prev.in + norm.inputTokens,
    out: prev.out + norm.outputTokens,
    usd: prev.usd + usd,
    calls: prev.calls + 1,
  };

  const total: JobCost = {
    usd: cur.usd + usd,
    tokens: {
      in: cur.tokens.in + norm.inputTokens,
      out: cur.tokens.out + norm.outputTokens,
    },
    byModel,
  };

  await store.set(costKey(args.jobId), total);
  await updateJobMeta(args.jobId, { estimatedCost: total.usd });

  return { usd, total };
}

// --- budget ----------------------------------------------------------------

export function deepBudgetUsd(): number {
  const raw = env("BUDGET_USD_PER_DEEP_JOB");
  const n = raw != null ? Number(raw) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 8.0;
}

// When the depth reviewer escalates a job to the pro model tier (gpt-5.4-pro
// at high effort + Gemini cross-review), it gets a larger budget so the extra
// passes can actually run. Defaults to ~2.5x the base cap.
export function deepEscalatedBudgetUsd(): number {
  const raw = env("BUDGET_USD_PER_DEEP_JOB_ESCALATED");
  const n = raw != null ? Number(raw) : NaN;
  if (Number.isFinite(n) && n > 0) return n;
  return Math.max(deepBudgetUsd() * 2.5, 20.0);
}

export function budgetState(usd: number, cap = deepBudgetUsd()): {
  utilization: number; // 0..1+
  remaining: number;
  capped: boolean;
  warning: boolean;
} {
  const utilization = usd / cap;
  return {
    utilization,
    remaining: Math.max(0, cap - usd),
    capped: usd >= cap,
    warning: utilization >= 0.85,
  };
}
