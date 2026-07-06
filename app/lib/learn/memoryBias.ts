// app/lib/learn/memoryBias.ts
//
// Recall-scoring "MicroLoRA-adapter spirit" nudge layer: a per-tenant,
// per-memory-kind (and per-solution-source) learned namespace weight,
// blended with a cross-tenant global fallback for cold-start tenants, that
// nudges memory/solution ranking on top of the existing deterministic score.
//
// Pure composition over the existing retrieveRelevantMemories/recallSolutions
// public APIs — memoryRetrieval.ts and solutionMemory.ts are never touched
// and remain the fallback path on any error or when learning is disabled.
//
// Redis schema:
//   learn:mem:ns:{tenantId}:{kind}   HASH  field "stat" -> StatRecord
//   learn:mem:ns:global:{kind}       HASH  field "stat" -> StatRecord
//   learn:sol:ns:{tenantId}:{source} HASH  field "stat" -> StatRecord
//   learn:sol:ns:global:{source}     HASH  field "stat" -> StatRecord

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";
import {
  learnDisabled as learnSubsystemDisabled,
  confidence,
  updateStat as updateStoredStat,
  DEFAULT_STAT,
  type StatRecord,
} from "@/app/lib/learn/stableStats";
import {
  retrieveRelevantMemories,
  type RetrievalOptions,
} from "@/app/lib/memoryRetrieval";
import {
  memoryDefaultRetrievalLimit,
  type MemoryEntry,
  type MemoryKind,
} from "@/app/lib/memoryStore";
import { recallSolutions, type SolutionSource } from "@/app/lib/solutionMemory";
import type { MemoryHit } from "@/app/lib/agentMemory";

// Confidence/update math is the SAME shared stableStats.ts primitive routerBias.ts
// uses (stability-weighted mean with a decayed effective-n and a permanent
// plasticity floor) — a tenant's signal from months ago doesn't outrank a
// fresher-but-thinner one forever, and enough data never fully freezes the mean.
const MIN_CONF = 0.5;

// Nudge is deliberately small relative to the existing score's [0,1]-ish
// range (wR+wK+wI+wA = 1.0) so it can only reorder items whose base scores
// were already close, never overpower a clear base-score winner.
const NUDGE_SCALE = 0.3;
const NUDGE_CLAMP = 0.15;

const WIDEN_FACTOR = 3;
const MEM_WIDEN_CAP = 24;

// Honors both this module's own granular override (MEMORY_LEARN=0) AND the
// subsystem-wide kill switch (LEARN_SUBSYSTEM=0, stableStats.ts) so a single
// env var can shut off every learn:* module at once for rollback safety.
function learnDisabled(): boolean {
  return learnSubsystemDisabled() || (env("MEMORY_LEARN") ?? "1") === "0";
}

function clamp(x: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, x));
}

// pipelineMany's raw HGET results may already be decoded (Upstash
// auto-parses JSON-looking strings) or still a raw string, depending on
// backend — accept either, falling back to DEFAULT_STAT (n=0 => confidence 0)
// on anything unrecognized.
function parseStat(raw: unknown): StatRecord {
  try {
    const val = typeof raw === "string" ? JSON.parse(raw) : raw;
    if (val && typeof (val as StatRecord).mean === "number" && typeof (val as StatRecord).n === "number") {
      return val as StatRecord;
    }
    return DEFAULT_STAT;
  } catch {
    return DEFAULT_STAT;
  }
}

// Tenant evidence wins once sufficient; else defer to the population signal;
// else neutral no-op (0.5 blended => zero nudge).
function blend(tenantStat: StatRecord, globalStat: StatRecord, now: number): number {
  if (confidence(tenantStat, now) >= MIN_CONF) return tenantStat.mean;
  if (confidence(globalStat, now) >= MIN_CONF) return globalStat.mean;
  return 0.5;
}

function nudgeFromBlend(blended: number): number {
  return clamp((blended - 0.5) * 2 * NUDGE_SCALE, -NUDGE_CLAMP, NUDGE_CLAMP);
}

async function safeUpdateStat(key: string, quality: number): Promise<void> {
  try {
    await updateStoredStat(getStore(), key, "stat", quality);
  } catch {
    // best-effort — outcome recording must never block the caller.
  }
}

function memNsKey(tenantId: string, kind: MemoryKind): string {
  return `learn:mem:ns:${tenantId}:${kind}`;
}
function memNsGlobalKey(kind: MemoryKind): string {
  return `learn:mem:ns:global:${kind}`;
}
function solNsKey(tenantId: string, source: SolutionSource): string {
  return `learn:sol:ns:${tenantId}:${source}`;
}
function solNsGlobalKey(source: SolutionSource): string {
  return `learn:sol:ns:global:${source}`;
}

// Module 4 (relation-graph) integration point: additive boost per candidate
// id. Returns an empty map — a safe no-op — until that module is wired up.
async function graphBoost(_tenantId: string, _candidateIds: string[]): Promise<Map<string, number>> {
  return new Map();
}

export async function retrieveRelevantMemoriesLearned(
  tenantId: string,
  opts: RetrievalOptions
): Promise<MemoryEntry[]> {
  if (learnDisabled()) return retrieveRelevantMemories(tenantId, opts);

  try {
    const originalLimit = opts.limit ?? memoryDefaultRetrievalLimit();
    // Never fetch FEWER than the caller asked for. MEM_WIDEN_CAP only bounds
    // how much EXTRA context we pull in to rerank over; if originalLimit is
    // already >= the cap (e.g. MEMORY_RETRIEVAL_LIMIT configured above 24),
    // min(originalLimit*WIDEN_FACTOR, MEM_WIDEN_CAP) could come out BELOW
    // originalLimit and silently truncate results relative to the
    // non-learned path. max(...) with originalLimit forecloses that.
    const widenedLimit = Math.max(originalLimit, Math.min(originalLimit * WIDEN_FACTOR, MEM_WIDEN_CAP));
    const candidates = await retrieveRelevantMemories(tenantId, { ...opts, limit: widenedLimit });
    if (candidates.length === 0) return await retrieveRelevantMemories(tenantId, opts);

    const kinds = Array.from(new Set(candidates.map((c) => c.kind)));
    const store = getStore();
    const cmds: (string | number)[][] = [];
    for (const kind of kinds) {
      cmds.push(["HGET", memNsKey(tenantId, kind), "stat"]);
      cmds.push(["HGET", memNsGlobalKey(kind), "stat"]);
    }
    const results = await store.pipelineMany(cmds);

    const now = Date.now();
    const nudgeByKind = new Map<MemoryKind, number>();
    kinds.forEach((kind, i) => {
      const tenantStat = parseStat(results[i * 2]);
      const globalStat = parseStat(results[i * 2 + 1]);
      nudgeByKind.set(kind, nudgeFromBlend(blend(tenantStat, globalStat, now)));
    });

    const boosts = await graphBoost(tenantId, candidates.map((c) => c.id));

    const ranked = candidates
      .map((entry, i) => {
        const baseScoreProxy = 1 - i / candidates.length;
        const combinedScore =
          baseScoreProxy + (nudgeByKind.get(entry.kind) ?? 0) + (boosts.get(entry.id) ?? 0);
        return { entry, combinedScore };
      })
      .sort((a, b) => b.combinedScore - a.combinedScore);

    return ranked.slice(0, originalLimit).map((r) => r.entry);
  } catch {
    return retrieveRelevantMemories(tenantId, opts);
  }
}

export async function recallSolutionsLearned(args: {
  tenantId: string;
  query: string;
  topK?: number;
  minScore?: number;
}): Promise<MemoryHit[]> {
  if (learnDisabled()) return recallSolutions(args);

  try {
    const originalTopK = args.topK ?? 3;
    const effectiveMinScore = args.minScore ?? Number(env("SOLUTION_MIN_SCORE") ?? "0.3");
    const hits = await recallSolutions({
      tenantId: args.tenantId,
      query: args.query,
      topK: originalTopK * WIDEN_FACTOR,
      minScore: args.minScore,
    });
    if (hits.length === 0) return await recallSolutions(args);

    const sources = Array.from(
      new Set(
        hits
          .map((h) => (h.record.meta as { source?: SolutionSource } | undefined)?.source)
          .filter((s): s is SolutionSource => !!s)
      )
    );
    const store = getStore();
    const cmds: (string | number)[][] = [];
    for (const source of sources) {
      cmds.push(["HGET", solNsKey(args.tenantId, source), "stat"]);
      cmds.push(["HGET", solNsGlobalKey(source), "stat"]);
    }
    const results = sources.length ? await store.pipelineMany(cmds) : [];

    const now = Date.now();
    const nudgeBySource = new Map<SolutionSource, number>();
    sources.forEach((source, i) => {
      const tenantStat = parseStat(results[i * 2]);
      const globalStat = parseStat(results[i * 2 + 1]);
      nudgeBySource.set(source, nudgeFromBlend(blend(tenantStat, globalStat, now)));
    });

    return hits
      .map((hit) => {
        const source = (hit.record.meta as { source?: SolutionSource } | undefined)?.source;
        const combinedScore = hit.score + (source ? nudgeBySource.get(source) ?? 0 : 0);
        return { hit, combinedScore };
      })
      .sort((a, b) => b.combinedScore - a.combinedScore)
      // Re-apply the ORIGINAL minScore to the ORIGINAL (un-nudged) score: the
      // nudge may only re-order candidates that already passed the
      // deterministic floor, never admit one the gate rejected.
      .filter((r) => r.hit.score >= effectiveMinScore)
      .slice(0, originalTopK)
      .map((r) => r.hit);
  } catch {
    return recallSolutions(args);
  }
}

export async function recordMemoryKindOutcome(
  tenantId: string,
  kind: MemoryKind,
  quality: number
): Promise<void> {
  await safeUpdateStat(memNsKey(tenantId, kind), quality);
  await safeUpdateStat(memNsGlobalKey(kind), quality);
}

export async function recordSolutionSourceOutcome(
  tenantId: string,
  source: SolutionSource,
  quality: number
): Promise<void> {
  await safeUpdateStat(solNsKey(tenantId, source), quality);
  await safeUpdateStat(solNsGlobalKey(source), quality);
}
