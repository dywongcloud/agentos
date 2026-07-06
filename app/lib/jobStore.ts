// app/lib/jobStore.ts
//
// Redis schema for long-running, observable, multi-tenant agent jobs.
//
// Key layout:
//   job:{jobId}:meta              JSON  { jobId, tenantId, channel, sessionId, prompt,
//                                         status, kind, createdAt, updatedAt, finishedAt,
//                                         resultText?, error?, parentJobId? }
//   job:{jobId}:snapshot          JSON  XState persisted snapshot for jobMachine
//   job:{jobId}:thoughts          List  newest-first; each entry is a JSON line
//                                       { ts, kind, text, data? }
//   job:{jobId}:artifacts         List  newest-first VFS path refs
//   job:{jobId}:children          Set   sub-job IDs
//   tenant:{tenantId}:jobs:active Set   currently in-flight job IDs
//   tenant:{tenantId}:jobs:recent List  newest-first job IDs (capped)
//
// tenantId convention: same as agentTurn's userId, channel-qualified
// (e.g. "telegram:123456789"). For dispatch from `/job` Telegram command we
// reuse this so VFS / Composio scopes line up.

import { getStore } from "@/app/lib/store";
import type { Channel } from "@/app/lib/identity";

// Use Web Crypto (available in workflow VM + Node 18+ + Edge runtime) rather
// than node:crypto, because this module is imported transitively by
// app/workflows/jobWorkflow.ts and Vercel Workflow DevKit forbids Node
// built-ins inside workflow functions.
function randomUuid(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto && typeof g.crypto.randomUUID === "function") {
    return g.crypto.randomUUID();
  }
  // Last-resort fallback — not cryptographically strong but the jobId is just
  // a routing key, not a security token.
  return (
    Date.now().toString(36) +
    Math.random().toString(36).slice(2) +
    Math.random().toString(36).slice(2)
  );
}

export type JobStatus =
  | "pending"
  | "clarifying"
  | "needs_input"
  | "planning"
  | "executing"
  | "verifying"
  | "revising"
  | "done"
  | "failed"
  | "cancelled";

export type JobKind = "chat" | "research" | "code" | "document" | "auto";

export type JobMeta = {
  jobId: string;
  tenantId: string;
  channel: Channel;
  sessionId: string;
  prompt: string;
  kind: JobKind;
  status: JobStatus;
  createdAt: number;
  updatedAt: number;
  startedAt?: number;
  finishedAt?: number;
  resultText?: string;
  resultArtifacts?: string[];
  error?: string;
  parentJobId?: string;
  // Running USD estimate from costTracker. Visible via /status so users can
  // see what their deep jobs are spending.
  estimatedCost?: number;
  // Deep-mode depth-reviewer state. `escalated` flips true once the reviewer
  // decides the job needs the top model tier (gpt-5.4-pro, high effort);
  // subsequent orchestrator + subtask calls read it. `depthPasses` counts how
  // many depth-driven extra passes we've run (separate budget from the
  // correctness reviseCount).
  escalated?: boolean;
  depthPasses?: number;
  // Sub-agent / workforce context: set when this job is a workforce member
  // turn, so the UI can join jobs to agents and team runs.
  agentId?: string;
  workforceRunId?: string;
};

export type Thought = {
  ts: number;
  kind:
    | "info"
    | "transition"
    | "tool"
    | "reasoning"
    | "observation"
    | "error"
    | "result";
  text: string;
  data?: Record<string, unknown>;
};

const RECENT_JOBS_CAP = 100;
const THOUGHTS_CAP = 500;

export function newJobId(): string {
  // 14-char total: "j_" prefix + 12 hex chars from a UUID. Short enough to
  // /status comfortably from Telegram.
  return "j_" + randomUuid().replace(/-/g, "").slice(0, 12);
}

function metaKey(jobId: string) {
  return `job:${jobId}:meta`;
}
function snapshotKey(jobId: string) {
  return `job:${jobId}:snapshot`;
}
function thoughtsKey(jobId: string) {
  return `job:${jobId}:thoughts`;
}
function artifactsKey(jobId: string) {
  return `job:${jobId}:artifacts`;
}
function childrenKey(jobId: string) {
  return `job:${jobId}:children`;
}
function activeJobsKey(tenantId: string) {
  return `tenant:${tenantId}:jobs:active`;
}
function recentJobsKey(tenantId: string) {
  return `tenant:${tenantId}:jobs:recent`;
}

export async function createJob(input: {
  tenantId: string;
  channel: Channel;
  sessionId: string;
  prompt: string;
  kind?: JobKind;
  parentJobId?: string;
  agentId?: string;
  workforceRunId?: string;
}): Promise<JobMeta> {
  const now = Date.now();
  const jobId = newJobId();
  const meta: JobMeta = {
    jobId,
    tenantId: input.tenantId,
    channel: input.channel,
    sessionId: input.sessionId,
    prompt: input.prompt,
    kind: input.kind ?? "auto",
    status: "pending",
    createdAt: now,
    updatedAt: now,
    parentJobId: input.parentJobId,
    agentId: input.agentId,
    workforceRunId: input.workforceRunId,
  };

  const store = getStore();
  await store.set(metaKey(jobId), meta);
  await store.sadd(activeJobsKey(input.tenantId), jobId);
  await store.lpush(recentJobsKey(input.tenantId), jobId);
  await store.ltrim(recentJobsKey(input.tenantId), 0, RECENT_JOBS_CAP - 1);
  if (input.parentJobId) {
    await store.sadd(childrenKey(input.parentJobId), jobId);
  }
  return meta;
}

export async function getJobMeta(jobId: string): Promise<JobMeta | null> {
  const store = getStore();
  return (await store.get<JobMeta>(metaKey(jobId))) ?? null;
}

export async function updateJobMeta(
  jobId: string,
  patch: Partial<JobMeta>
): Promise<JobMeta | null> {
  const store = getStore();
  const cur = await getJobMeta(jobId);
  if (!cur) return null;
  const next: JobMeta = { ...cur, ...patch, updatedAt: Date.now() };
  await store.set(metaKey(jobId), next);
  if (
    patch.status === "done" ||
    patch.status === "failed" ||
    patch.status === "cancelled"
  ) {
    await store.srem(activeJobsKey(cur.tenantId), jobId);
  }
  return next;
}

export async function appendThought(
  jobId: string,
  thought: Omit<Thought, "ts"> & { ts?: number }
): Promise<void> {
  const store = getStore();
  const t: Thought = { ts: thought.ts ?? Date.now(), ...thought };
  await store.lpush(thoughtsKey(jobId), JSON.stringify(t));
  await store.ltrim(thoughtsKey(jobId), 0, THOUGHTS_CAP - 1);
}

export async function getThoughts(
  jobId: string,
  opts?: { limit?: number }
): Promise<Thought[]> {
  const store = getStore();
  const limit = Math.max(1, Math.min(THOUGHTS_CAP, opts?.limit ?? 50));
  const raw = await store.lrange(thoughtsKey(jobId), 0, limit - 1);
  const out: Thought[] = [];
  for (const r of raw) {
    try {
      out.push(JSON.parse(r) as Thought);
    } catch {
      // skip malformed
    }
  }
  return out;
}

export async function saveSnapshot(
  jobId: string,
  snapshot: unknown
): Promise<void> {
  const store = getStore();
  await store.set(snapshotKey(jobId), snapshot as never);
}

export async function loadSnapshot<T = unknown>(
  jobId: string
): Promise<T | null> {
  const store = getStore();
  return (await store.get<T>(snapshotKey(jobId))) ?? null;
}

export async function addArtifact(jobId: string, ref: string): Promise<void> {
  const store = getStore();
  await store.lpush(artifactsKey(jobId), ref);
}

export async function listArtifacts(jobId: string): Promise<string[]> {
  const store = getStore();
  return store.lrange(artifactsKey(jobId), 0, -1);
}

export async function listActiveJobs(tenantId: string): Promise<string[]> {
  const store = getStore();
  return store.smembers(activeJobsKey(tenantId));
}

// Mark every active job for a tenant cancelled (used by /stop and /start). The
// job workflow polls its own status between steps and halts when it sees this,
// so a long deep run stops promptly. Returns the ids that were cancelled.
export async function cancelActiveJobs(
  tenantId: string,
  reason = "halted by /stop"
): Promise<string[]> {
  const ids = await listActiveJobs(tenantId);
  for (const id of ids) {
    await updateJobMeta(id, { status: "cancelled" });
    await appendThought(id, { kind: "info", text: reason });
  }
  return ids;
}

export async function listRecentJobs(
  tenantId: string,
  limit = 20
): Promise<string[]> {
  const store = getStore();
  return store.lrange(recentJobsKey(tenantId), 0, Math.max(0, limit - 1));
}

export async function listChildren(jobId: string): Promise<string[]> {
  const store = getStore();
  return store.smembers(childrenKey(jobId));
}
