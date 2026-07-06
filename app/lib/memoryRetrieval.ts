// app/lib/memoryRetrieval.ts
//
// Deterministic retrieval — no LLM, just Redis + a scoring formula. Same
// input always returns the same output, which is the user's stated goal.
//
// Scoring:
//   score = recencyBoost + keywordBoost + importanceBoost + accessBoost
//
//   recencyBoost      = 1.0 when within last day, decays linearly to 0 at 30d
//   keywordBoost      = 1.0 × hit-rate of message tokens vs (title+labels+summary)
//   importanceBoost   = entry.importance (0..1)
//   accessBoost       = log10(accessCount + 1) clamped at 1.0
//
// Tunable weights via env: MEM_W_RECENCY, MEM_W_KEYWORD, MEM_W_IMPORTANCE,
// MEM_W_ACCESS.

import {
  listRecent,
  listByKind,
  type MemoryEntry,
  memoryDefaultRetrievalLimit,
} from "@/app/lib/memoryStore";
import { env } from "@/app/lib/env";

const DAY_MS = 24 * 60 * 60 * 1000;
const RECENCY_HORIZON_MS = 30 * DAY_MS;

function weight(name: string, dflt: number): number {
  const v = Number(env(name) ?? "");
  return Number.isFinite(v) && v >= 0 ? v : dflt;
}

function tokenize(s: string): Set<string> {
  return new Set(
    (s || "")
      .toLowerCase()
      .split(/[^a-z0-9_/.-]+/)
      .filter((t) => t.length >= 2 && t.length <= 32)
  );
}

function recencyBoost(lastAccessedAt: number, now: number): number {
  const ageMs = Math.max(0, now - lastAccessedAt);
  if (ageMs <= DAY_MS) return 1.0;
  if (ageMs >= RECENCY_HORIZON_MS) return 0;
  return 1 - (ageMs - DAY_MS) / (RECENCY_HORIZON_MS - DAY_MS);
}

function keywordBoost(
  entry: MemoryEntry,
  queryTokens: Set<string>,
  saturation: number
): number {
  if (queryTokens.size === 0) return 0;
  const text = [
    entry.title,
    entry.summary ?? "",
    entry.labels.join(" "),
    entry.content,
  ]
    .join(" ")
    .toLowerCase();
  let hits = 0;
  for (const t of queryTokens) if (text.includes(t)) hits++;
  // Saturating hit-COUNT, not hit-rate over query size. The old `hits /
  // queryTokens.size` diluted the score for longer / multi-sentence queries
  // (a 30-word message that matched on the one token that mattered scored
  // ~0.03), which made recall worse exactly when the user gave more context.
  // Counting raw overlaps and saturating at `saturation` tokens means a richer
  // query can only ever ADD signal — never penalize — so it's safe to feed in
  // recent-turn / scope context at the call site.
  return Math.min(1, hits / Math.max(1, saturation));
}

export type RetrievalOptions = {
  query?: string;
  limit?: number;
};

export async function retrieveRelevantMemories(
  tenantId: string,
  opts: RetrievalOptions = {}
): Promise<MemoryEntry[]> {
  const limit = opts.limit ?? memoryDefaultRetrievalLimit();
  // Pull a candidate pool wider than the final limit, then re-rank. Recent
  // entries are the most-likely-relevant baseline.
  const candidates = await listRecent(tenantId, Math.max(limit * 4, 24));
  if (candidates.length === 0) return [];

  const wR = weight("MEM_W_RECENCY", 0.4);
  const wK = weight("MEM_W_KEYWORD", 0.3);
  const wI = weight("MEM_W_IMPORTANCE", 0.2);
  const wA = weight("MEM_W_ACCESS", 0.1);
  const kwSat = weight("MEM_KEYWORD_SAT", 3);
  const now = Date.now();

  const queryTokens = tokenize(opts.query ?? "");

  const scored = candidates.map((e) => {
    const r = recencyBoost(e.lastAccessedAt, now);
    const k = keywordBoost(e, queryTokens, kwSat);
    const i = e.importance;
    const a = Math.min(1, Math.log10((e.accessCount || 0) + 1));
    const score = wR * r + wK * k + wI * i + wA * a;
    return { e, score };
  });

  scored.sort((a, b) => b.score - a.score);
  return scored.slice(0, limit).map((s) => s.e);
}

// Compact natural-language block to drop into the system prompt every turn.
// Keeps tokens cheap by line-formatting and trimming aggressively.
export function memoriesToPromptBlock(memories: MemoryEntry[]): string {
  if (memories.length === 0) return "";
  const lines = memories.map((m) => {
    const parts = [`• [${m.kind}] ${m.title}`];
    if (m.summary) parts.push(`  ${m.summary}`);
    if (m.kind === "directory" && m.fields?.path) parts.push(`  path: ${m.fields.path}`);
    if (m.kind === "command" && m.fields?.command) parts.push(`  cmd: ${m.fields.command}`);
    if (m.labels.length) parts.push(`  tags: ${m.labels.join(", ")}`);
    return parts.join("\n");
  });
  return [
    "USER MEMORY (most recently relevant entries — use to ground answers, do not repeat verbatim unless asked):",
    lines.join("\n"),
  ].join("\n");
}

// Also export a "kind-grouped" view useful for /memories Telegram command.
export async function memorySummary(
  tenantId: string,
  perKind = 5
): Promise<{
  total: number;
  groups: Array<{ kind: string; entries: MemoryEntry[] }>;
}> {
  const groups: Array<{ kind: string; entries: MemoryEntry[] }> = [];
  let total = 0;
  for (const kind of [
    "preference",
    "fact",
    "directory",
    "command",
    "workflow",
    "project",
    "person",
    "code_snippet",
    "credential_hint",
    "other",
  ] as const) {
    const entries = await listByKind(tenantId, kind, perKind);
    total += entries.length;
    if (entries.length) groups.push({ kind, entries });
  }
  return { total, groups };
}
