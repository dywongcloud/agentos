// app/lib/codeProjectStore.ts
//
// Persistent state for long-running /code projects. Mirrors jobStore's Redis
// schema but keeps the project model separate because:
//
//   - A "project" is durable across many tasks (turn 1 starts it, turn N
//     continues it). A "job" is a single dispatch.
//   - Projects hold workdir + engine + auth-snapshot metadata that jobs don't.
//   - Different lifecycle: a project can sit `idle` for hours/days between
//     turns and be re-attached; a job runs once and finalizes.
//
// Key layout:
//   codeproj:{projectId}:meta              JSON  CodeProjectMeta
//   codeproj:{projectId}:log               List  newest-first thought entries
//   codeproj:{projectId}:tasks             List  newest-first task records
//   tenant:{tenantId}:codeprojs:active     Set   in-flight project IDs
//   tenant:{tenantId}:codeprojs:recent     List  newest-first project IDs
//
// Tenant scoping: same convention as jobStore — `${channel}:${senderId}`.

import { getStore } from "@/app/lib/store";
import type { Channel } from "@/app/lib/identity";

function randomId(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto && typeof g.crypto.randomUUID === "function") {
    return g.crypto.randomUUID();
  }
  return (
    Date.now().toString(36) +
    Math.random().toString(36).slice(2) +
    Math.random().toString(36).slice(2)
  );
}

// Project IDs are short and human-pasteable from Telegram, e.g. "p_a1b2c3d4e5".
export function newProjectId(): string {
  return "p_" + randomId().replace(/-/g, "").slice(0, 10);
}

export type CodeEngine = "claude" | "opencode";

export type CodeProjectStatus =
  | "pending"
  | "working"
  | "awaiting_followup" // done with a turn, ready for /code attach
  | "done" // explicitly finalized — e.g. pushed
  | "failed";

export type CodeProjectMeta = {
  projectId: string;
  tenantId: string;
  channel: Channel;
  sessionId: string;
  title: string;
  engine: CodeEngine;
  status: CodeProjectStatus;
  createdAt: number;
  updatedAt: number;
  // Sandbox path the project's workdir lives at. Stable across turns so
  // `claude --continue` finds its session history.
  sandboxWorkdir: string;
  // VFS root for materialized output — /workspace/claude_code/projects/{projectId}
  vfsRoot: string;
  // The current/most-recent task body. Full task history is in the tasks list.
  currentTask?: string;
  // Optional repo flow info if the project is bound to a GitHub repo.
  repoUrl?: string;
  baseBranch?: string;
  pushedBranch?: string;
  // Rolling total of how many turns this project has had — useful for naming
  // output files (output-1.md, output-2.md, ...).
  turnCount: number;
  // Last claude/opencode stdout chunk; tail-rendered by /code status.
  lastOutput?: string;
  // Last failure message if any. Cleared on the next successful turn.
  lastError?: string;
};

export type CodeProjectThought = {
  ts: number;
  kind:
    | "info"
    | "transition"
    | "tool"
    | "engine_stdout"
    | "result"
    | "error";
  text: string;
  data?: Record<string, unknown>;
};

export type CodeProjectTaskRecord = {
  turn: number;
  ts: number;
  task: string;
  status: "done" | "failed";
  outputPreview?: string; // first ~600 chars of stdout for /code status
  error?: string;
};

const RECENT_PROJECTS_CAP = 50;
const LOG_CAP = 400;
const TASKS_CAP = 200;

function metaKey(id: string) {
  return `codeproj:${id}:meta`;
}
function logKey(id: string) {
  return `codeproj:${id}:log`;
}
function tasksKey(id: string) {
  return `codeproj:${id}:tasks`;
}
function activeKey(tid: string) {
  return `tenant:${tid}:codeprojs:active`;
}
function recentKey(tid: string) {
  return `tenant:${tid}:codeprojs:recent`;
}

// ---------------------------------------------------------------------------
// CRUD
// ---------------------------------------------------------------------------

export async function createCodeProject(input: {
  tenantId: string;
  channel: Channel;
  sessionId: string;
  title: string;
  engine: CodeEngine;
  sandboxWorkdir: string;
  vfsRoot: string;
  repoUrl?: string;
  baseBranch?: string;
}): Promise<CodeProjectMeta> {
  const now = Date.now();
  const projectId = newProjectId();
  const meta: CodeProjectMeta = {
    projectId,
    tenantId: input.tenantId,
    channel: input.channel,
    sessionId: input.sessionId,
    title: input.title.slice(0, 200),
    engine: input.engine,
    status: "pending",
    createdAt: now,
    updatedAt: now,
    sandboxWorkdir: input.sandboxWorkdir,
    vfsRoot: input.vfsRoot,
    repoUrl: input.repoUrl,
    baseBranch: input.baseBranch,
    turnCount: 0,
  };

  const store = getStore();
  await store.set(metaKey(projectId), meta);
  await store.sadd(activeKey(input.tenantId), projectId);
  await store.lpush(recentKey(input.tenantId), projectId);
  await store.ltrim(recentKey(input.tenantId), 0, RECENT_PROJECTS_CAP - 1);
  return meta;
}

export async function getCodeProject(
  projectId: string
): Promise<CodeProjectMeta | null> {
  const store = getStore();
  return (await store.get<CodeProjectMeta>(metaKey(projectId))) ?? null;
}

export async function updateCodeProject(
  projectId: string,
  patch: Partial<CodeProjectMeta>
): Promise<CodeProjectMeta | null> {
  const store = getStore();
  const cur = await getCodeProject(projectId);
  if (!cur) return null;
  const next: CodeProjectMeta = { ...cur, ...patch, updatedAt: Date.now() };
  await store.set(metaKey(projectId), next);
  if (patch.status === "done" || patch.status === "failed") {
    await store.srem(activeKey(cur.tenantId), projectId);
  } else if (patch.status === "working" || patch.status === "pending") {
    await store.sadd(activeKey(cur.tenantId), projectId);
  }
  return next;
}

export async function appendCodeThought(
  projectId: string,
  t: Omit<CodeProjectThought, "ts"> & { ts?: number }
): Promise<void> {
  const store = getStore();
  const entry: CodeProjectThought = { ts: t.ts ?? Date.now(), ...t };
  await store.lpush(logKey(projectId), JSON.stringify(entry));
  await store.ltrim(logKey(projectId), 0, LOG_CAP - 1);
}

export async function getCodeThoughts(
  projectId: string,
  opts?: { limit?: number }
): Promise<CodeProjectThought[]> {
  const store = getStore();
  const limit = Math.max(1, Math.min(LOG_CAP, opts?.limit ?? 50));
  const raw = await store.lrange(logKey(projectId), 0, limit - 1);
  const out: CodeProjectThought[] = [];
  for (const r of raw) {
    try {
      out.push(JSON.parse(r) as CodeProjectThought);
    } catch {
      // skip malformed
    }
  }
  return out;
}

export async function appendCodeTask(
  projectId: string,
  rec: CodeProjectTaskRecord
): Promise<void> {
  const store = getStore();
  await store.lpush(tasksKey(projectId), JSON.stringify(rec));
  await store.ltrim(tasksKey(projectId), 0, TASKS_CAP - 1);
}

export async function getCodeTasks(
  projectId: string,
  limit = 20
): Promise<CodeProjectTaskRecord[]> {
  const store = getStore();
  const raw = await store.lrange(tasksKey(projectId), 0, Math.max(0, limit - 1));
  const out: CodeProjectTaskRecord[] = [];
  for (const r of raw) {
    try {
      out.push(JSON.parse(r) as CodeProjectTaskRecord);
    } catch {
      // skip
    }
  }
  return out;
}

export async function listActiveCodeProjects(tid: string): Promise<string[]> {
  const store = getStore();
  return store.smembers(activeKey(tid));
}

export async function listRecentCodeProjects(
  tid: string,
  limit = 10
): Promise<string[]> {
  const store = getStore();
  return store.lrange(recentKey(tid), 0, Math.max(0, limit - 1));
}
