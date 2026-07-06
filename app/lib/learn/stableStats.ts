import type { Store } from "@/app/lib/store";
import { env } from "@/app/lib/env";

export type StatRecord = { mean: number; n: number; updatedAt: number };

export const DEFAULT_STAT: StatRecord = { mean: 0.5, n: 0, updatedAt: 0 };

const DAY_MS = 24 * 60 * 60 * 1000;

function clamp(x: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, x));
}

function clamp01(x: number): number {
  return clamp(x, 0, 1);
}

export function decayedN(rec: StatRecord, now: number, halfLifeMs: number = 14 * DAY_MS): number {
  const elapsed = Math.max(0, now - rec.updatedAt);
  return rec.n * Math.pow(0.5, elapsed / halfLifeMs);
}

export function stabilityWeightedUpdate(
  rec: StatRecord,
  x: number,
  opts?: { now?: number; halfLifeMs?: number; nCap?: number; minAlpha?: number; maxAlpha?: number }
): StatRecord {
  const now = opts?.now ?? Date.now();
  const halfLifeMs = opts?.halfLifeMs ?? 14 * DAY_MS;
  const nCap = opts?.nCap ?? 200;
  const effN = decayedN(rec, now, halfLifeMs);
  const alpha = clamp(1 / (Math.min(effN, nCap) + 1), opts?.minAlpha ?? 0.01, opts?.maxAlpha ?? 0.35);
  const newMean = rec.mean + alpha * (clamp01(x) - rec.mean);
  const newN = Math.min(rec.n + 1, nCap * 1.5);
  return { mean: newMean, n: newN, updatedAt: now };
}

export function confidence(rec: StatRecord, now: number, opts?: { halfLifeMs?: number; kappa?: number }): number {
  const effN = decayedN(rec, now, opts?.halfLifeMs ?? 14 * DAY_MS);
  const kappa = opts?.kappa ?? 6;
  return effN / (effN + kappa);
}

export async function readStat(store: Store, key: string, field: string): Promise<StatRecord> {
  try {
    const raw = await store.hget<string>(key, field);
    if (!raw || typeof raw !== "string") return DEFAULT_STAT;
    const parsed = JSON.parse(raw);
    if (
      typeof parsed?.mean !== "number" ||
      typeof parsed?.n !== "number" ||
      typeof parsed?.updatedAt !== "number"
    ) {
      return DEFAULT_STAT;
    }
    return parsed as StatRecord;
  } catch {
    return DEFAULT_STAT;
  }
}

export async function writeStat(store: Store, key: string, field: string, rec: StatRecord): Promise<void> {
  try {
    await store.hset(key, field, JSON.stringify(rec));
  } catch {
    // swallow: write failures must never surface to callers
  }
}

export async function updateStat(
  store: Store,
  key: string,
  field: string,
  x: number,
  opts?: { now?: number; halfLifeMs?: number; nCap?: number; minAlpha?: number; maxAlpha?: number }
): Promise<StatRecord> {
  try {
    const cur = await readStat(store, key, field);
    const next = stabilityWeightedUpdate(cur, x, opts);
    await writeStat(store, key, field, next);
    return next;
  } catch {
    return DEFAULT_STAT;
  }
}

export function learnDisabled(): boolean {
  return env("LEARN_SUBSYSTEM") === "0";
}
