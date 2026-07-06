// app/lib/modelCatalog.ts
//
// A curated catalog of the LLMs an agent can be pointed at via the model
// picker. This is presentation metadata only — the actual provider routing is
// still owned by modelRouting.ts (providerFor / resolveModel turn any of these
// ids into a live LanguageModel). Keeping the catalog separate lets the UI show
// provider, context window, credit tier and capabilities without baking those
// facts into the routing chokepoint.
//
// `id` is the exact string passed to resolveModel(): bare OpenAI ids
// ("gpt-5.4"), bare Anthropic ids that modelRouting prefixes for the gateway
// ("claude-opus-4.8"), and Gemini ids ("gemini-3.1-pro-preview").

import { providerFor, type ModelProvider } from "@/app/lib/modelRouting";

export type CreditTier = "low" | "moderate" | "high";

export type ModelInfo = {
  id: string;
  label: string;
  provider: ModelProvider; // derived, but stored for cheap UI access
  vendor: string; // human label: "OpenAI" | "Anthropic" | "Google"
  description: string;
  contextWindow: number; // tokens
  outputLimit: number; // tokens
  creditTier: CreditTier;
  reasoning: boolean; // exposes a "thinking" / reasoning-effort mode
  recommended?: boolean;
};

const VENDOR_LABEL: Record<ModelProvider, string> = {
  openai: "OpenAI",
  google: "Google",
  gateway: "Anthropic",
  tencent: "DeepSeek",
  anthropic: "Anthropic",
};

// Raw catalog — keep newest/strongest first within each vendor so the default
// "Recommended" sort reads sensibly.
const RAW: Omit<ModelInfo, "provider" | "vendor">[] = [
  // ── OpenAI ────────────────────────────────────────────────────────────
  {
    id: "gpt-5.5",
    label: "GPT 5.5",
    description:
      "OpenAI's latest flagship. Strong multi-step tool use and well-formatted output.",
    contextWindow: 1_050_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
    recommended: true,
  },
  {
    id: "gpt-5.5-pro",
    label: "GPT 5.5 Pro",
    description: "Extended-reasoning variant of 5.5 for the hardest synthesis tasks.",
    contextWindow: 1_050_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "gpt-5.4",
    label: "GPT 5.4",
    description:
      "The default workhorse — reliable agentic tool calling at a moderate cost.",
    contextWindow: 1_050_000,
    outputLimit: 128_000,
    creditTier: "moderate",
    reasoning: true,
    recommended: true,
  },
  {
    id: "gpt-5.4-pro",
    label: "GPT 5.4 Pro",
    description: "Higher-reasoning 5.4 for heavy synthesis and planning.",
    contextWindow: 1_050_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "gpt-5.4-mini",
    label: "GPT 5.4 mini",
    description: "Fast and cheap — good for triage, routing and light tasks.",
    contextWindow: 400_000,
    outputLimit: 128_000,
    creditTier: "low",
    reasoning: false,
  },
  {
    id: "gpt-5.3-codex",
    label: "GPT 5.3 Codex",
    description: "Coding-tuned with long reasoning traces and strong structured output.",
    contextWindow: 1_050_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "gpt-5.2",
    label: "GPT 5.2",
    description: "Prior-generation flagship. Capable, slightly cheaper than 5.4.",
    contextWindow: 400_000,
    outputLimit: 128_000,
    creditTier: "moderate",
    reasoning: true,
  },
  {
    id: "gpt-4.1",
    label: "GPT 4.1",
    description: "Lightweight, low-latency model suited to search and simple lookups.",
    contextWindow: 1_000_000,
    outputLimit: 32_000,
    creditTier: "low",
    reasoning: false,
  },
  {
    id: "o3",
    label: "o3",
    description: "Dedicated reasoning model for planning and verification.",
    contextWindow: 200_000,
    outputLimit: 100_000,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "o3-pro",
    label: "o3 Pro",
    description: "Maximum-effort reasoning for the most demanding problems.",
    contextWindow: 200_000,
    outputLimit: 100_000,
    creditTier: "high",
    reasoning: true,
  },

  // ── Anthropic (via the Vercel AI Gateway) ─────────────────────────────
  {
    id: "claude-opus-4.8",
    label: "Claude Opus 4.8",
    description:
      "Anthropic's Opus with adaptive thinking. Excellent reasoning, coding and agentic work.",
    contextWindow: 1_000_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
    recommended: true,
  },
  {
    id: "claude-opus-4.7",
    label: "Claude Opus 4.7",
    description: "Prior Opus release — strong general-purpose reasoning and writing.",
    contextWindow: 1_000_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "claude-opus-4.6",
    label: "Claude Opus 4.6",
    description: "Opus 4.6 — dependable complex reasoning and long-context handling.",
    contextWindow: 1_000_000,
    outputLimit: 128_000,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "claude-sonnet-4.6",
    label: "Claude Sonnet 4.6",
    description: "Balanced Anthropic model — fast, capable, lower cost than Opus.",
    contextWindow: 1_000_000,
    outputLimit: 64_000,
    creditTier: "moderate",
    reasoning: true,
  },

  // ── Google ────────────────────────────────────────────────────────────
  {
    id: "gemini-3.1-pro-preview",
    label: "Gemini 3.1 Pro Preview",
    description: "Google's Pro model with a very large context window and native vision.",
    contextWindow: 1_048_576,
    outputLimit: 65_536,
    creditTier: "high",
    reasoning: true,
  },
  {
    id: "gemini-3.5-flash",
    label: "Gemini 3.5 Flash",
    description: "Very fast, very cheap Google model for high-volume light tasks.",
    contextWindow: 1_048_576,
    outputLimit: 65_536,
    creditTier: "low",
    reasoning: false,
  },
];

export const MODEL_CATALOG: ModelInfo[] = RAW.map((m) => ({
  ...m,
  provider: providerFor(m.id),
  vendor: VENDOR_LABEL[providerFor(m.id)],
}));

const BY_ID = new Map(MODEL_CATALOG.map((m) => [m.id, m]));

export function getModelInfo(id: string | undefined | null): ModelInfo | undefined {
  if (!id) return undefined;
  return BY_ID.get(id);
}

// A model id is acceptable for persistence if it's in the catalog. We keep this
// strict so the picker can't smuggle an arbitrary/unsupported string into an
// agent record; callers that want a raw override still go through env routing.
export function isKnownModel(id: string | undefined | null): id is string {
  return !!id && BY_ID.has(id);
}
