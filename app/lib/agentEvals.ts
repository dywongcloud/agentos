// app/lib/agentEvals.ts
//
// Per-agent eval scores + governed self-optimization experiments.
//
// This is the measurement + governance layer behind the "L4 self-driving"
// vision: every meaningful agent run can be scored across named dimensions
// (e.g. "Email quality", "Lead qualification accuracy"); those scores power
// the eval graph in the dashboard AND gate the self-optimization loop. The
// loop proposes an experiment (a persona/skills tweak), A/B-tests the
// candidate against the current baseline on the same inputs, and ONLY
// promotes the candidate when its eval score beats the baseline by a margin
// — so the agent improves itself without ever regressing below what's proven.
//
// Redis layout (no TTL — standing history):
//   agenteval:scores:{agentId}   LIST  AgentEvalScore JSON, newest-first, capped
//   agenteval:exp:{id}           JSON  AgentExperiment
//   agenteval:exps:{agentId}     LIST  experiment ids, newest-first, capped
//
// Workflow-reachable: no Node builtins, JSON-serializable types only.

import { getStore } from "@/app/lib/store";

// --- types ------------------------------------------------------------------

export type EvalDimension = {
  name: string; // human label, e.g. "Email quality"
  score: number; // 0..100
};

export type AgentEvalScore = {
  id: string;
  tenantId: string;
  agentId: string;
  overall: number; // 0..100, mean of dimensions
  dimensions: EvalDimension[];
  threshold: number; // pass line (default 90)
  runId?: string; // automation run that produced the scored output
  experimentId?: string; // set when this score came from an A/B test arm
  arm?: "baseline" | "candidate";
  note?: string; // grader's one-line rationale
  durationMs?: number; // wall-clock of the scored run (long-running task evals)
  ts: number;
};

export type AgentExperimentChange = {
  persona?: string; // replacement persona text
  skills?: string[]; // replacement skill allowlist
};

export type AgentExperiment = {
  id: string;
  tenantId: string;
  agentId: string;
  hypothesis: string; // what the optimizer thinks will improve
  change: AgentExperimentChange; // the candidate variant's diff vs. baseline
  margin: number; // required overall improvement to promote (default 3)
  baselineScore?: number;
  candidateScore?: number;
  status: "proposed" | "testing" | "promoted" | "rejected";
  decisionNote?: string;
  createdAt: number;
  decidedAt?: number;
};

export const DEFAULT_THRESHOLD = 90;
export const DEFAULT_MARGIN = 3;

// --- keys -------------------------------------------------------------------

const scoresKey = (agentId: string) => `agenteval:scores:${agentId}`;
const expKey = (id: string) => `agenteval:exp:${id}`;
const expsKey = (agentId: string) => `agenteval:exps:${agentId}`;

const SCORES_CAP = 400;
const EXPS_CAP = 200;

function shortId(prefix: string): string {
  return prefix + "_" + globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 12);
}

function meanOf(dims: EvalDimension[]): number {
  if (!dims.length) return 0;
  const sum = dims.reduce((a, d) => a + (Number.isFinite(d.score) ? d.score : 0), 0);
  return Math.round((sum / dims.length) * 10) / 10;
}

// --- scores -----------------------------------------------------------------

export async function putAgentEvalScore(
  s: Omit<AgentEvalScore, "id" | "overall" | "ts" | "threshold"> & {
    id?: string;
    overall?: number;
    ts?: number;
    threshold?: number;
  }
): Promise<AgentEvalScore> {
  const store = getStore();
  const full: AgentEvalScore = {
    id: s.id ?? shortId("aev"),
    tenantId: s.tenantId,
    agentId: s.agentId,
    dimensions: s.dimensions,
    overall: s.overall ?? meanOf(s.dimensions),
    threshold: s.threshold ?? DEFAULT_THRESHOLD,
    runId: s.runId,
    experimentId: s.experimentId,
    arm: s.arm,
    note: s.note,
    ...(typeof s.durationMs === "number" ? { durationMs: s.durationMs } : {}),
    ts: s.ts ?? Date.now(),
  };
  await store.lpush(scoresKey(full.agentId), JSON.stringify(full));
  await store.ltrim(scoresKey(full.agentId), 0, SCORES_CAP - 1);
  return full;
}

export async function listAgentEvalScores(
  agentId: string,
  limit = 200
): Promise<AgentEvalScore[]> {
  const raw = await getStore().lrange(scoresKey(agentId), 0, limit - 1);
  const out: AgentEvalScore[] = [];
  for (const r of raw) {
    try {
      out.push(JSON.parse(r) as AgentEvalScore);
    } catch {
      // skip malformed
    }
  }
  return out;
}

export async function latestAgentEvalScore(
  agentId: string
): Promise<AgentEvalScore | null> {
  const [s] = await listAgentEvalScores(agentId, 1);
  return s ?? null;
}

// Bucket scores into the last `weeks` ISO weeks (most recent last). Each bucket
// is the mean overall of the scores that fall in it; empty weeks carry the
// previous bucket's value forward so the bar chart has no holes. Feeds the
// "Overall eval score — Last 12 weeks" view.
export function weeklyOverall(
  scores: AgentEvalScore[],
  weeks = 12,
  now = Date.now()
): Array<{ label: string; value: number | null }> {
  const WEEK = 7 * 24 * 60 * 60 * 1000;
  const buckets: Array<{ sum: number; n: number }> = Array.from(
    { length: weeks },
    () => ({ sum: 0, n: 0 })
  );
  for (const s of scores) {
    const ageWeeks = Math.floor((now - s.ts) / WEEK);
    if (ageWeeks < 0 || ageWeeks >= weeks) continue;
    const idx = weeks - 1 - ageWeeks; // oldest at index 0, newest at end
    buckets[idx]!.sum += s.overall;
    buckets[idx]!.n += 1;
  }
  const out: Array<{ label: string; value: number | null }> = [];
  let carry: number | null = null;
  for (let i = 0; i < weeks; i++) {
    const b = buckets[i]!;
    const v: number | null = b.n > 0 ? Math.round((b.sum / b.n) * 10) / 10 : carry;
    if (b.n > 0) carry = v;
    out.push({ label: `W${i + 1}`, value: v });
  }
  return out;
}

// --- experiments ------------------------------------------------------------

export async function putAgentExperiment(
  e: Omit<AgentExperiment, "id" | "createdAt" | "status" | "margin"> & {
    id?: string;
    createdAt?: number;
    status?: AgentExperiment["status"];
    margin?: number;
  }
): Promise<AgentExperiment> {
  const store = getStore();
  const isNew = !e.id;
  const full: AgentExperiment = {
    id: e.id ?? shortId("aexp"),
    tenantId: e.tenantId,
    agentId: e.agentId,
    hypothesis: e.hypothesis,
    change: e.change,
    margin: e.margin ?? DEFAULT_MARGIN,
    baselineScore: e.baselineScore,
    candidateScore: e.candidateScore,
    status: e.status ?? "proposed",
    decisionNote: e.decisionNote,
    createdAt: e.createdAt ?? Date.now(),
    decidedAt: e.decidedAt,
  };
  await store.set(expKey(full.id), full);
  if (isNew) {
    await store.lpush(expsKey(full.agentId), full.id);
    await store.ltrim(expsKey(full.agentId), 0, EXPS_CAP - 1);
  }
  return full;
}

export async function getAgentExperiment(
  id: string
): Promise<AgentExperiment | null> {
  return getStore().get<AgentExperiment>(expKey(id));
}

export async function listAgentExperiments(
  agentId: string,
  limit = 50
): Promise<AgentExperiment[]> {
  const ids = await getStore().lrange(expsKey(agentId), 0, limit - 1);
  const out: AgentExperiment[] = [];
  for (const id of ids) {
    const e = await getAgentExperiment(id);
    if (e) out.push(e);
  }
  return out;
}

// Governance rule: a candidate only wins if it clears the baseline by the
// configured margin. Ties and regressions keep the proven baseline.
export function shouldPromote(
  baselineScore: number | undefined,
  candidateScore: number | undefined,
  margin: number
): boolean {
  if (candidateScore == null) return false;
  const base = baselineScore ?? 0;
  return candidateScore - base >= margin;
}
