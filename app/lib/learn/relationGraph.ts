// app/lib/learn/relationGraph.ts
//
// Co-occurrence graph between memory ids that were attributed together to a
// good-outcome turn. OPTIONAL / lowest-value component of the learn stack:
// write side (recordMemoryCooccurrence) is safe to enable immediately since
// it has no effect until graphBoost() is actually composed into retrieval
// scoring — hold off wiring the read side until edge density (SCARD growth
// under learn:graph:*) looks meaningful for a given tenant.

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";
import { learnDisabled as learnSubsystemDisabled } from "@/app/lib/learn/stableStats";

const DECAY = 0.97;
const MAX_EDGES = 8;
const TRIM_PROBABILITY = 0.1;
const MAX_PAIR_IDS = 6;
const MAX_CANDIDATES = 24;
const MAX_BOOST = 0.1;
const MIN_QUALITY = 0.55;
// Refreshed on every write so an edge only survives ~90 days of total
// inactivity — bounds the learn:graph:* key space to recently-relevant
// memory ids instead of growing forever with no TTL.
const EDGE_TTL_SECONDS = 90 * 24 * 60 * 60;

// Honors both this module's own override (LEARN_DISABLED=true) AND the
// subsystem-wide kill switch (LEARN_SUBSYSTEM=0, stableStats.ts).
function learnDisabled(): boolean {
  return learnSubsystemDisabled() || env("LEARN_DISABLED") === "true";
}

function edgeKey(tenantId: string, memId: string): string {
  return `learn:graph:${tenantId}:${memId}`;
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

export async function recordMemoryCooccurrence(
  tenantId: string,
  memoryIds: string[],
  quality: number
): Promise<void> {
  try {
    if (learnDisabled()) return;
    if (quality < MIN_QUALITY) return;
    if (!Array.isArray(memoryIds) || memoryIds.length < 2) return;

    const ids = memoryIds.slice(0, MAX_PAIR_IDS);
    const pairs: Array<[string, string]> = [];
    for (let i = 0; i < ids.length; i++) {
      for (let j = i + 1; j < ids.length; j++) {
        if (ids[i] === ids[j]) continue;
        pairs.push([ids[i], ids[j]]);
      }
    }
    if (pairs.length === 0) return;

    const store = getStore();

    const oldScores = await store.pipelineMany(
      pairs.map(([a, b]) => ["ZSCORE", edgeKey(tenantId, a), b])
    );

    // EWC-spirit stability: decay-in-place instead of overwrite, so one
    // nudge can never zero out an edge that many prior turns built up.
    const writeCmds: (string | number)[][] = [];
    const touchedKeys = new Set<string>();
    for (let i = 0; i < pairs.length; i++) {
      const [a, b] = pairs[i];
      const newScore = (Number(oldScores[i]) || 0) * DECAY + 1;
      writeCmds.push(["ZADD", edgeKey(tenantId, a), newScore, b]);
      writeCmds.push(["ZADD", edgeKey(tenantId, b), newScore, a]);
      touchedKeys.add(edgeKey(tenantId, a));
      touchedKeys.add(edgeKey(tenantId, b));
    }
    // Refresh TTL on every touched key so the key space is self-cleaning
    // (bounded Redis key growth — no key survives indefinitely once its
    // memory ids stop being co-attributed to good outcomes).
    for (const key of touchedKeys) {
      writeCmds.push(["EXPIRE", key, EDGE_TTL_SECONDS]);
    }
    await store.pipelineMany(writeCmds);

    if (Math.random() < TRIM_PROBABILITY) {
      const touched = Array.from(new Set(ids));
      await store.pipelineMany(
        touched.map((id) => ["ZREMRANGEBYRANK", edgeKey(tenantId, id), 0, -(MAX_EDGES + 1)])
      );
    }
  } catch {
    // Nudge layer: never let a co-occurrence write break the caller.
  }
}

export async function graphBoost(
  tenantId: string,
  candidateIds: string[]
): Promise<Map<string, number>> {
  try {
    if (!Array.isArray(candidateIds) || candidateIds.length === 0) return new Map();

    const candidates = candidateIds.slice(0, MAX_CANDIDATES);
    const store = getStore();

    const results = await store.pipelineMany(
      candidates.map((c) => ["ZRANGE", edgeKey(tenantId, c), 0, -1, "WITHSCORES"])
    );

    const edgesByCandidate = new Map<string, Map<string, number>>();
    let maxWeight = 0;
    for (let i = 0; i < candidates.length; i++) {
      const flat = Array.isArray(results[i]) ? (results[i] as unknown[]) : [];
      const edges = new Map<string, number>();
      for (let j = 0; j + 1 < flat.length; j += 2) {
        const weight = Number(flat[j + 1]);
        edges.set(String(flat[j]), weight);
        if (weight > maxWeight) maxWeight = weight;
      }
      edgesByCandidate.set(candidates[i], edges);
    }

    const boosts = new Map<string, number>();
    for (const c of candidates) {
      if (maxWeight <= 0) {
        boosts.set(c, 0);
        continue;
      }
      const edges = edgesByCandidate.get(c);
      let rawSum = 0;
      for (const other of candidates) {
        if (other === c) continue;
        const w = edges?.get(other);
        if (w) rawSum += w;
      }
      // Divide by the max edge weight seen in this batch so the boost stays
      // a comparable [0, MAX_BOOST] nudge regardless of a tenant's graph density.
      boosts.set(c, clamp(rawSum / maxWeight, 0, MAX_BOOST));
    }

    return boosts;
  } catch {
    return new Map();
  }
}
