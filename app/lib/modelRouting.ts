// app/lib/modelRouting.ts
//
// Purpose-based model routing. Each "purpose" is a semantic role in the
// pipeline; the actual model name behind it is configurable via env so the
// system can be re-pointed at new releases without code changes.
//
// Mapping (defaults match the user's stated preference):
//
//   chat            plain non-/job Telegram chat                  gpt-5.4
//   fast-meta       cheap meta-decisions: clarifier, depth
//                   classifier, /ask side-channel, orchestrator's
//                   small "is this done?" check                   gpt-5.4-mini
//   meta            deep-mode orchestrator thinking step          gpt-5.4
//   meta-pro        deep-mode revise/near-end synthesis           gpt-5.3-codex
//   smart           default /job executor (agentTurn)             gpt-5.4
//   smart-pro       heavy synthesis stages inside deep mode       gpt-5.4-pro
//   coding          executor when modality is code-*              gpt-5.3-codex
//   reasoning       planner + verifier on normal /job             o3
//   reasoning-pro   deep-mode orchestrator + final synthesis      o3-pro
//   search          web-search subagent                           gpt-4.1
//
// All slots fall back through a sensible chain if the env var is unset
// (smart-pro → smart, reasoning-pro → reasoning, coding → smart, etc.) so
// missing access to premium models degrades cleanly rather than breaking.

import { openai, createOpenAI } from "@ai-sdk/openai";
import { google } from "@ai-sdk/google";
import { createAnthropic } from "@ai-sdk/anthropic";
import type { LanguageModel } from "ai";
import { env } from "@/app/lib/env";

// --- Anthropic (Claude Fable 5) ------------------------------------------------
//
// When ANTHROPIC_API_KEY is set, Claude Fable 5 (the most capable available
// model) takes over the DEEP/pro-tier slots and complex plain-language agentic
// chat turns:
//   - meta-pro / smart-pro / reasoning-pro — the /deep orchestrator's revise,
//     heavy-synthesis, and final-synthesis stages
//   - plain (non-/command) chat messages that look like complex multi-tool
//     work (see looksComplexAgentic + chatModelNameFor)
// Explicit per-slot env overrides (META_PRO_MODEL_NAME etc.) always win, and
// with no key everything falls back to the existing OpenAI models untouched.
// Claude names route to the direct Anthropic API when the key exists;
// otherwise they still resolve via the Vercel AI Gateway as before.
// Disable without removing the key via FABLE_DISABLED=1.

const FABLE_DEFAULT_MODEL = "claude-fable-5";
// Regular/ad-hoc agentic work (complex plain-language chat turns with Composio
// tool selection + tool calling) runs on Sonnet, NOT Fable — Fable is reserved
// for the /deep pro-tier slots. Verified against the live models list;
// override via AGENTIC_CHAT_MODEL_NAME.
const AGENTIC_CHAT_DEFAULT_MODEL = "claude-sonnet-5";

export function fableEnabled(): boolean {
  return !!env("ANTHROPIC_API_KEY") && (env("FABLE_DISABLED") ?? "0") !== "1";
}

export function fableModelName(): string {
  return env("FABLE_MODEL_NAME") ?? FABLE_DEFAULT_MODEL;
}

export function agenticChatModelName(): string {
  return env("AGENTIC_CHAT_MODEL_NAME") ?? AGENTIC_CHAT_DEFAULT_MODEL;
}

let _anthropic: ReturnType<typeof createAnthropic> | null = null;
function anthropicProvider() {
  if (!_anthropic) {
    _anthropic = createAnthropic({ apiKey: env("ANTHROPIC_API_KEY") ?? "" });
  }
  return _anthropic;
}

// Deep/pro-tier purposes that Fable takes over when enabled.
const FABLE_DEEP_PURPOSES = new Set(["meta-pro", "smart-pro", "reasoning-pro"]);

// --- Tencent TokenHub (DeepSeek) ---------------------------------------------
//
// TokenHub is an OpenAI-compatible gateway hosting DeepSeek. When
// TOKENHUB_API_KEY is set, DeepSeek backs the interactive CHAT slot ONLY — the
// plain conversational agent the user talks to (see chatModelName). It is
// deliberately kept OUT of every agentic / tool-driven path:
//   - /job + automation executors (agentTurn) — tool calling, planning
//   - the fast-meta slot (clarifier, depth classifier, "is this done?" checks,
//     the automation compiler) — deciding WHICH tools/steps to run
//   - deep-mode orchestration and workflows
//   - text-only auxiliary calls (summarize/compact/enrich) via textAuxModel,
//     which run inside those jobs/automations
// DeepSeek is weaker at tool calling, so those slots stay on their OpenAI
// (gpt-5.4-class / reasoning / codex) or gateway defaults. Chat still routes to
// DeepSeek by resolving the bare "deepseek-*" name through the tencent branch
// of resolveModel below — no name remap needed.
//
// DeepSeek is TEXT-ONLY — never route vision/audio/TTS/STT here.

const TOKENHUB_DEFAULT_BASE_URL = "https://tokenhub-intl.tencentcloudmaas.com/v1";
// Only deepseek-v3.2 is currently served on this key; override via
// TOKENHUB_MODEL once a V4 slug is provisioned.
const TOKENHUB_DEFAULT_MODEL = "deepseek-v3.2";

export function tokenhubEnabled(): boolean {
  return !!env("TOKENHUB_API_KEY");
}

let _tokenhub: ReturnType<typeof createOpenAI> | null = null;
function tokenhubProvider() {
  if (!_tokenhub) {
    _tokenhub = createOpenAI({
      apiKey: env("TOKENHUB_API_KEY") ?? "",
      baseURL: env("TOKENHUB_BASE_URL") ?? TOKENHUB_DEFAULT_BASE_URL,
    });
  }
  return _tokenhub;
}

// Build the DeepSeek LanguageModel. MUST use .chat() — the provider's default
// callable / languageModel() target OpenAI's Responses API (/v1/responses),
// which the TokenHub gateway does NOT implement (it only serves
// /v1/chat/completions). Calling it the default way 404s every request.
function tokenhubModel(id: string): LanguageModel {
  return tokenhubProvider().chat(id);
}

export function tokenhubModelId(): string {
  return env("TOKENHUB_MODEL") ?? TOKENHUB_DEFAULT_MODEL;
}

// The model name to use for a given purpose, honoring the TokenHub override.
// Chat is re-pointed at DeepSeek when TokenHub is enabled so the interactive
// agent the user talks to runs on DeepSeek too (not just background/aux slots).
export function chatModelName(): string {
  if (tokenhubEnabled()) return tokenhubModelId();
  return resolveModelName("chat");
}

// Deterministic "does this plain-language message look like complex multi-tool
// agentic work?" gate. Zero tokens — pure heuristics over the text. Three
// signal categories are evaluated independently; escalation requires AT LEAST
// 2 of the 3 to fire so that a single incidental keyword never triggers the
// escalation alone:
//
//   Signal A — multi-app:      2+ distinct app/data-noun mentions
//                              (gmail+sheets, slack+notion, …)
//   Signal B — action verb:    any tool-invocation verb present
//                              (send, create, schedule, automate, sync, …)
//   Signal C — multi-step:     any sequencing connector present
//                              (then, after that, for each, first … then, …)
//
// Two-signal minimum: both (A+B), (A+C), or (B+C) must be present to return
// true. A single signal in isolation always returns false.
//
// Test cases (signal counts shown):
//   "hi there"                                     → 0 signals → false
//   "send an email"                                → B only    → false
//   "then update the sheet"                        → B+C       → true  ✓
//   "send email via gmail then update sheet"       → A+B+C     → true  ✓
//   "gmail and notion"                             → A only    → false
//   "schedule a meeting then send a slack message" → A+B+C     → true  ✓
const COMPLEX_APP_WORDS =
  /(gmail|e-?mail|inbox|sheet|spreadsheet|slack|calendar|notion|github|drive|docs?\b|monday|hubspot|salesforce|linear|jira|airtable|contacts?|crm|webhook|database|csv)/gi;
const COMPLEX_ACTION_VERBS =
  /\b(send|create|update|append|schedule|draft|fetch|pull|sync|compile|generate|post|move|delete|search|find|summari[sz]e|index|log|track|remind|automate|loop|scrape|extract|merge|import|export)\b/gi;
const COMPLEX_SEQUENCE =
  /\b(then|after (?:that|which)|for (?:each|every)|each row|every row|one by one|step \d|first,|finally|once (?:that|it)'?s? done)\b/i;

export function looksComplexAgentic(text: string | null | undefined): boolean {
  const t = (text ?? "").trim();
  if (!t || t.startsWith("/")) return false;

  // Count distinct app/data-noun matches (lowercased to deduplicate variants).
  const appCount = new Set((t.match(COMPLEX_APP_WORDS) ?? []).map((s) => s.toLowerCase())).size;
  const verbCount = new Set((t.match(COMPLEX_ACTION_VERBS) ?? []).map((s) => s.toLowerCase())).size;

  // Signal A fires only when 2+ distinct apps are mentioned (multi-app).
  const hasMultiApp = appCount >= 2;
  // Signal B fires on any tool-invocation verb.
  const hasActionVerb = verbCount >= 1;
  // Signal C fires on any multi-step sequencing connector.
  const hasSequence = COMPLEX_SEQUENCE.test(t);

  const signals = [hasMultiApp, hasActionVerb, hasSequence].filter(Boolean).length;
  return signals >= 2;
}

// Chat model for a SPECIFIC inbound message: complex plain-language agentic
// requests (multi-app, multi-step — i.e. regular ad-hoc Composio tool
// selection + tool calling) escalate to Claude SONNET when available. NOT
// Fable — Fable is reserved for /deep pro-tier orchestration; Sonnet is the
// right cost/latency tier for ad-hoc tool work. Everything else keeps the
// normal chat model (DeepSeek/TokenHub or gpt-5.4). With no ANTHROPIC_API_KEY
// this is exactly chatModelName() — clean fallback.
export function chatModelNameFor(text: string | null | undefined): string {
  if (fableEnabled() && looksComplexAgentic(text)) return agenticChatModelName();
  return chatModelName();
}

// Resolve a model for a cheap, text-only auxiliary call. These run inside
// jobs/automations (summarize, compact, enrich), so DeepSeek is intentionally
// NOT used here — it's confined to the interactive chat slot (chatModelName).
// The fallback name resolves through the normal chokepoint.
export function textAuxModel(fallbackModelName: string): LanguageModel {
  return resolveModel(fallbackModelName);
}

export type Purpose =
  | "chat"
  | "fast-meta"
  | "meta"
  | "meta-pro"
  | "smart"
  | "smart-pro"
  | "coding"
  | "reasoning"
  | "reasoning-pro"
  | "search"
  // browser-pro: Gemini 3.1 Pro acting as a reasoning side-car for browser
  // operations — it enriches the raw goal into a multi-step navigation plan
  // and re-plans mid-browse. Separate from `smart` so the browser brain can
  // be re-pointed independently.
  | "browser-pro";

export type ReasoningEffort = "low" | "medium" | "high";

// --- defaults ----------------------------------------------------------------

const DEFAULTS: Record<Purpose, string> = {
  // Basic Telegram chat runs on the smart workhorse (gpt-5.4). Earlier this
  // sat on gemini-3.5-flash / gpt-4.1 for speed+cost, but those models were
  // too weak at multi-step tool calling and producing well-formatted,
  // styled output (e.g. populating a Google Doc/Sheet from structured data).
  // The user wants chat to match the /job executor's quality. Override via
  // CHAT_MODEL_NAME — but do NOT point it at gemini-3.5-flash or gpt-4.1 for
  // this tenant; keep it at gpt-5.4-class or better.
  chat: "gpt-5.4",
  "fast-meta": "gpt-5.4-mini",
  // Deep-mode orchestrator tier. We deliberately do NOT use gpt-5.2 here
  // any more — its reasoning quality on the orchestrator's planning loop
  // was poor enough that runs stalled / failed to make progress. We sit
  // on gpt-5.4 for the regular thinking step and escalate the "revise /
  // near-end synthesis" slot to gpt-5.3-codex (its longer reasoning
  // traces + structured output handling were a better fit than 5.2-pro
  // for the bounded-iteration synthesis pattern). Override via
  // META_MODEL_NAME / META_PRO_MODEL_NAME.
  meta: "gpt-5.4",
  "meta-pro": "gpt-5.3-codex",
  smart: "gpt-5.4",
  "smart-pro": "gpt-5.4-pro",
  coding: "gpt-5.3-codex",
  reasoning: "o3",
  "reasoning-pro": "o3-pro",
  search: "gpt-4.1",
  // Gemini 3.1 Pro drives browser planning/enrichment. Google exposes it as
  // the "-preview" id (verified against the live models list); override via
  // BROWSER_PRO_MODEL_NAME if/when a GA id ships.
  "browser-pro": "gemini-3.1-pro-preview",
};

// Env var names for each purpose.
const ENV_KEYS: Record<Purpose, string> = {
  chat: "CHAT_MODEL_NAME",
  "fast-meta": "FAST_META_MODEL_NAME",
  meta: "META_MODEL_NAME",
  "meta-pro": "META_PRO_MODEL_NAME",
  smart: "SMART_MODEL_NAME",
  "smart-pro": "SMART_PRO_MODEL_NAME",
  coding: "CODING_MODEL_NAME",
  reasoning: "REASONING_MODEL_NAME",
  "reasoning-pro": "REASONING_PRO_MODEL_NAME",
  search: "SEARCH_MODEL_NAME",
  "browser-pro": "BROWSER_PRO_MODEL_NAME",
};

// Fallback chain — what to use if the configured model for a purpose is
// unavailable. Walked in order; first non-empty wins.
const FALLBACK_CHAIN: Record<Purpose, Purpose[]> = {
  chat: ["chat", "smart"],
  "fast-meta": ["fast-meta", "smart"],
  // meta + meta-pro are used by the deep-mode orchestrator for
  // thinking-heavy work. As of the gpt-5.2 deprecation in /deep + /job,
  // meta resolves to gpt-5.4 and meta-pro to gpt-5.3-codex by default;
  // both fall through to smart when the dedicated env isn't set.
  meta: ["meta", "smart"],
  "meta-pro": ["meta-pro", "meta", "smart"],
  smart: ["smart"],
  "smart-pro": ["smart-pro", "smart"],
  coding: ["coding", "smart"],
  reasoning: ["reasoning", "smart"],
  "reasoning-pro": ["reasoning-pro", "reasoning", "smart"],
  search: ["search", "smart"],
  // browser-pro falls back to smart (an OpenAI model) when no Gemini key /
  // browser model is configured, so browsing keeps working without Gemini.
  "browser-pro": ["browser-pro", "smart"],
};

// --- resolution -------------------------------------------------------------

export function resolveModelName(purpose: Purpose): string {
  // Fable takes over the deep/pro-tier slots when available — but an explicit
  // env override for the slot always wins, so operators keep full control.
  if (fableEnabled() && FABLE_DEEP_PURPOSES.has(purpose) && !env(ENV_KEYS[purpose])) {
    return fableModelName();
  }
  // Legacy compatibility: MODEL_NAME used to be the universal override.
  const legacy = env("MODEL_NAME");
  for (const p of FALLBACK_CHAIN[purpose]) {
    const v = env(ENV_KEYS[p]);
    if (v) return v;
  }
  if (legacy) return legacy;
  return DEFAULTS[purpose];
}

export function reasoningEffort(): ReasoningEffort {
  const raw = (env("REASONING_EFFORT") ?? "high").toLowerCase();
  if (raw === "low" || raw === "medium" || raw === "high") return raw;
  return "high";
}

// Which provider owns a model name. Prefix-based so adding a model is a
// data change, not a code change. Gemini → Google; "provider/model" ids and
// claude-*/fable-* names → Vercel AI Gateway (ai@6's global provider accepts
// plain strings and routes them through the gateway — covers Anthropic,
// Fable, and any other vendor without adding per-provider SDKs or API keys;
// on Vercel deployments auth is automatic via OIDC). Everything else → OpenAI.
export type ModelProvider = "openai" | "google" | "gateway" | "tencent" | "anthropic";

// Bare model-name prefixes that belong to gateway vendors, mapped to the
// gateway's provider slug — lets callers say "claude-opus-4.8" instead of
// "anthropic/claude-opus-4.8".
const GATEWAY_VENDOR_PREFIXES: Array<[RegExp, string]> = [
  [/^claude[-.]/, "anthropic"],
  [/^fable[-.]/, "fable"],
];

export function providerFor(modelName: string): ModelProvider {
  const n = String(modelName ?? "").trim().toLowerCase();
  // DeepSeek names belong to the Tencent TokenHub gateway.
  if (n.startsWith("deepseek")) return "tencent";
  if (n.startsWith("gemini") || n.startsWith("google/") || n.startsWith("models/gemini")) {
    return "google";
  }
  // Claude/Fable: direct Anthropic API when the key is configured; otherwise
  // fall back to the Vercel AI Gateway route (pre-existing behavior).
  if ((/^claude[-.]/.test(n) || /^fable[-.]/.test(n)) && env("ANTHROPIC_API_KEY")) {
    return "anthropic";
  }
  if (n.includes("/")) return "gateway";
  if (GATEWAY_VENDOR_PREFIXES.some(([re]) => re.test(n))) return "gateway";
  return "openai";
}

// Canonical gateway id for a model name: pass "provider/model" through as-is,
// prefix bare claude-*/fable-* names with their vendor slug.
export function gatewayId(modelName: string): string {
  const n = String(modelName ?? "").trim();
  if (n.includes("/")) return n;
  const hit = GATEWAY_VENDOR_PREFIXES.find(([re]) => re.test(n.toLowerCase()));
  return hit ? `${hit[1]}/${n}` : n;
}

// Resolve a model NAME into an AI-SDK LanguageModel for the right provider.
// This is the single chokepoint that lets the rest of the codebase keep
// passing plain model-name strings around while transparently supporting
// OpenAI, Gemini, and gateway-routed vendors (Anthropic, Fable, …).
export function resolveModel(modelName: string): LanguageModel {
  const provider = providerFor(modelName);
  if (provider === "tencent") {
    // A bare "deepseek-*" name resolves to the TokenHub gateway. This is how
    // the chat slot reaches DeepSeek (chatModelName returns this name when
    // TokenHub is enabled). If the key is unset this still constructs a
    // provider with an empty key — but bare deepseek names are only ever
    // chosen when tokenhubEnabled(), so that path isn't reached in practice.
    return tokenhubModel(modelName);
  }
  if (provider === "anthropic") {
    // Direct Anthropic API (Claude Fable 5 etc.) using ANTHROPIC_API_KEY.
    return anthropicProvider()(modelName);
  }
  if (provider === "google") {
    // Strip any "google/" or "models/" prefix the caller might pass.
    const id = modelName.replace(/^google\//, "").replace(/^models\//, "");
    return google(id);
  }
  if (provider === "gateway") {
    // ai@6: a plain "provider/model" string IS a valid LanguageModel — the
    // global provider resolves it through the Vercel AI Gateway.
    return gatewayId(modelName);
  }
  return openai(modelName);
}

export function isReasoningModel(modelName: string): boolean {
  const n = String(modelName ?? "").trim().toLowerCase();
  // Only OpenAI models take `reasoningEffort` via providerOptions.openai.
  // Gemini and gateway-routed models (Claude, Fable, …) must be treated as
  // plain even when their names end in "-pro" / contain "thinking".
  if (providerFor(n) !== "openai") return false;
  // NOTE: deliberately no blanket "-pro" suffix check — gpt-5.4-pro / o3-pro are
  // already caught by the family prefixes, and a non-reasoning override like a
  // hypothetical "gpt-4-pro" would otherwise get reasoningEffort (rejected) and
  // lose its temperature.
  return (
    /^gpt-5(?:[.-]|$)/.test(n) ||
    /^o[134](?:[.-]|$)/.test(n) ||
    n.includes("reasoning") ||
    n.includes("-thinking")
  );
}

// --- ai-sdk args builder ----------------------------------------------------

type JsonScalar = string | number | boolean | null;
type JsonValue = JsonScalar | JsonValue[] | { [k: string]: JsonValue };

export type LlmArgs = {
  model: LanguageModel;
  modelName: string;
  providerOptions?: Record<string, Record<string, JsonValue>>;
  temperature?: number;
};

// Build the spreadable args for a purpose. Handles reasoning-model quirks:
//   - OpenAI reasoning models: no temperature, reasoning_effort via
//     providerOptions.openai
//   - Gemini: plain temperature, no reasoningEffort
export function buildLlmArgs(args: {
  purpose: Purpose;
  temperature?: number;
}): LlmArgs {
  const modelName = resolveModelName(args.purpose);

  // Note: DeepSeek is NOT used for any purpose slot here (including fast-meta,
  // which drives the clarifier / depth classifier / automation compiler). It is
  // confined to the interactive chat slot — see chatModelName + the header.
  const reasoning = isReasoningModel(modelName);

  const out: LlmArgs = {
    model: resolveModel(modelName),
    modelName,
  };

  if (reasoning) {
    out.providerOptions = {
      openai: { reasoningEffort: reasoningEffort() },
    };
  } else if (typeof args.temperature === "number" && Number.isFinite(args.temperature)) {
    out.temperature = args.temperature;
  }

  return out;
}

// Convenience: resolve a model name given a heuristic preference. Used by
// agentTurn-style callers that want "use the coding model for code, the
// smart model otherwise."
export function purposeForModality(modality: string | null | undefined): Purpose {
  if (!modality) return "smart";
  if (modality.startsWith("code-")) return "coding";
  return "smart";
}
