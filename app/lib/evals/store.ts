// app/lib/evals/store.ts
//
// Persistent, queryable eval storage on top of Redis. Designed to feel
// relational: cases and runs are addressable by id, and secondary
// sorted-set indexes give us "list recent runs by suite / case /
// deploy / status" without table scans.
//
// Key layout (all under the `nx_evals:` prefix so they're easy to find
// in Redis ops and obviously separate from app data):
//
//   nx_evals:case:{caseId}            JSON           EvalCase
//   nx_evals:run:{runId}              JSON           EvalRun
//   nx_evals:idx:case:by_suite:{s}    SET            case ids
//   nx_evals:idx:runs:by_suite:{s}    ZSET (-ts)     run ids, newest first
//   nx_evals:idx:runs:by_case:{c}     ZSET (-ts)     run ids, newest first
//   nx_evals:idx:runs:by_deploy:{d}   ZSET (-ts)     run ids, newest first
//   nx_evals:idx:runs:by_status:{s}   ZSET (-ts)     run ids, newest first
//   nx_evals:suites                   SET            suite names ever seen
//
// **No TTL** on any key. Upstash's default eviction policy on paid
// plans is `noeviction` for non-TTL keys, so eval data persists
// indefinitely. If we ever migrate to a tighter eviction policy, the
// `nx_evals:` prefix lets us special-case these keys.
//
// Scores are stored as `-Date.now()` so a ZRANGEBYSCORE from
// `-Infinity` to `0` returns members newest-first directly, without
// needing ZREVRANGEBYSCORE (which isn't on our Store interface).

import { getStore } from "@/app/lib/store";
import type { EvalCase, EvalRun } from "./types";

const PREFIX = "nx_evals";

const caseKey = (id: string) => `${PREFIX}:case:${id}`;
const runKey = (id: string) => `${PREFIX}:run:${id}`;
const caseBySuiteKey = (suite: string) => `${PREFIX}:idx:case:by_suite:${suite}`;
const runsBySuiteKey = (suite: string) => `${PREFIX}:idx:runs:by_suite:${suite}`;
const runsByCaseKey = (caseId: string) => `${PREFIX}:idx:runs:by_case:${caseId}`;
const runsByDeployKey = (deployId: string) =>
  `${PREFIX}:idx:runs:by_deploy:${deployId}`;
const runsByStatusKey = (status: string) =>
  `${PREFIX}:idx:runs:by_status:${status}`;
const suitesKey = `${PREFIX}:suites`;

function newId(prefix: "er" | "ec"): string {
  return `${prefix}_${Date.now().toString(36)}${Math.random()
    .toString(36)
    .slice(2, 8)}`;
}

// --- cases --------------------------------------------------------------

export async function putCase(
  c: Omit<EvalCase, "id" | "createdAt"> & { id?: string; createdAt?: number }
): Promise<EvalCase> {
  const store = getStore();
  const full: EvalCase = {
    id: c.id ?? newId("ec"),
    suite: c.suite,
    name: c.name,
    triggers: c.triggers,
    graders: c.graders,
    seedInput: c.seedInput,
    createdAt: c.createdAt ?? Date.now(),
  };
  await store.set(caseKey(full.id), full);
  await store.sadd(caseBySuiteKey(full.suite), full.id);
  await store.sadd(suitesKey, full.suite);
  return full;
}

export async function getCase(id: string): Promise<EvalCase | null> {
  return getStore().get<EvalCase>(caseKey(id));
}

export async function listCasesBySuite(suite: string): Promise<EvalCase[]> {
  const store = getStore();
  const ids = await store.smembers(caseBySuiteKey(suite));
  const out: EvalCase[] = [];
  for (const id of ids) {
    const c = await getCase(id);
    if (c) out.push(c);
  }
  return out;
}

// --- runs ---------------------------------------------------------------

export async function putRun(
  r: Omit<EvalRun, "id" | "ts"> & { id?: string; ts?: number }
): Promise<EvalRun> {
  const store = getStore();
  const ts = r.ts ?? Date.now();
  const full: EvalRun = {
    id: r.id ?? newId("er"),
    caseId: r.caseId,
    suite: r.suite,
    ts,
    deployId: r.deployId,
    input: r.input,
    actual: r.actual,
    grades: r.grades,
    status: r.status,
    jobId: r.jobId,
  };
  await store.set(runKey(full.id), full);
  // Negative score → ZRANGEBYSCORE asc returns newest first.
  const score = -ts;
  await store.zadd(runsBySuiteKey(full.suite), score, full.id);
  await store.zadd(runsByCaseKey(full.caseId), score, full.id);
  if (full.deployId) {
    await store.zadd(runsByDeployKey(full.deployId), score, full.id);
  }
  await store.zadd(runsByStatusKey(full.status), score, full.id);
  await store.sadd(suitesKey, full.suite);
  return full;
}

export async function getRun(id: string): Promise<EvalRun | null> {
  return getStore().get<EvalRun>(runKey(id));
}

// Hard-delete a run record and remove it from every secondary index.
// Returns true if a record existed and was removed.
export async function deleteRun(id: string): Promise<boolean> {
  const store = getStore();
  const r = await getRun(id);
  if (!r) return false;
  await store.zrem(runsBySuiteKey(r.suite), id);
  await store.zrem(runsByCaseKey(r.caseId), id);
  if (r.deployId) await store.zrem(runsByDeployKey(r.deployId), id);
  await store.zrem(runsByStatusKey(r.status), id);
  await store.del(runKey(id));
  return true;
}

export type ListRunsOpts = {
  suite?: string;
  caseId?: string;
  deployId?: string;
  status?: EvalRun["status"];
  limit?: number;
  // Newest run with ts >= sinceMs is included; older are skipped.
  sinceMs?: number;
};

// Resolve which index to walk, then load full run records. The first
// non-null filter wins; if no filter is set, walks the "by_suite:auto"
// index as a sensible default (auto-logged runs are the bulk of data).
export async function listRuns(opts: ListRunsOpts = {}): Promise<EvalRun[]> {
  const store = getStore();
  const limit = Math.max(1, Math.min(500, opts.limit ?? 50));

  let indexKey: string;
  if (opts.caseId) indexKey = runsByCaseKey(opts.caseId);
  else if (opts.deployId) indexKey = runsByDeployKey(opts.deployId);
  else if (opts.status) indexKey = runsByStatusKey(opts.status);
  else if (opts.suite) indexKey = runsBySuiteKey(opts.suite);
  else indexKey = runsBySuiteKey("auto");

  // Pull a generous candidate window so post-filtering doesn't truncate.
  const candidates = await store.zrangebyscore(
    indexKey,
    Number.NEGATIVE_INFINITY,
    0,
    { limit: limit * 4 }
  );

  const out: EvalRun[] = [];
  for (const id of candidates) {
    const r = await getRun(id);
    if (!r) continue;
    if (opts.sinceMs && r.ts < opts.sinceMs) continue;
    if (opts.status && r.status !== opts.status) continue;
    if (opts.suite && r.suite !== opts.suite) continue;
    if (opts.caseId && r.caseId !== opts.caseId) continue;
    if (opts.deployId && r.deployId !== opts.deployId) continue;
    out.push(r);
    if (out.length >= limit) break;
  }
  return out;
}

// --- suites summary -----------------------------------------------------

export async function listSuites(): Promise<string[]> {
  const store = getStore();
  const s = await store.smembers(suitesKey);
  s.sort();
  return s;
}

export type SuiteSummary = {
  suite: string;
  totalRunsRecent: number;
  passCount: number;
  failCount: number;
  partialCount: number;
  errorCount: number;
  passRate: number;
  lastRunTs?: number;
};

// Walks the last N runs per suite (default 50) to compute a pass-rate
// snapshot. Cheap: just decode the index ids + load each run JSON once.
export async function suiteSummary(
  suite: string,
  recentN = 50
): Promise<SuiteSummary> {
  const recent = await listRuns({ suite, limit: recentN });
  let pass = 0,
    fail = 0,
    partial = 0,
    err = 0;
  for (const r of recent) {
    if (r.status === "pass") pass++;
    else if (r.status === "fail") fail++;
    else if (r.status === "partial") partial++;
    else err++;
  }
  return {
    suite,
    totalRunsRecent: recent.length,
    passCount: pass,
    failCount: fail,
    partialCount: partial,
    errorCount: err,
    passRate: recent.length > 0 ? pass / recent.length : 0,
    lastRunTs: recent[0]?.ts,
  };
}
