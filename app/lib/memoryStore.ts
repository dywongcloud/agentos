// app/lib/memoryStore.ts
//
// Per-tenant long-term memory store. Redis-backed, deterministic reads.
//
// Design constraints (set by the user):
//   1. Reads are idempotent + deterministic — no LLM in the hot path.
//   2. Dense, structured entries — many small typed memories beat one giant
//      blob. Each entry carries summary + labels so retrieval can be
//      keyword-matched cheaply.
//   3. Tunable — env knobs for caps and retrieval counts.
//   4. System-aware — distinct memory KINDS for directories and commands so
//      the agent can answer "what command did I use to start the proxy?" or
//      "where do I keep my notes?" without a vector store.
//
// Redis schema (single tenant scope):
//   mem:{tid}:e:{id}              JSON entry
//   mem:{tid}:idx:recent          ZSET  member=id  score=lastAccessedMs
//   mem:{tid}:idx:kind:{kind}     ZSET  member=id  score=lastAccessedMs
//   mem:{tid}:idx:tag:{tag}       SET   of ids
//   mem:{tid}:idx:all             ZSET  member=id  score=createdAtMs
//
// Indices are maintained on every write/touch/delete so read ops are pure
// Redis range/intersect queries — no scans, no LLM, predictable cost.

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";

export type MemoryKind =
  | "directory" // a path the user works in (/workspace/foo, ~/.config, etc.)
  | "command" // a shell command or script invocation pattern
  | "preference" // "user prefers TypeScript", "user uses pnpm"
  | "fact" // assertion about the user / their stack
  | "code_snippet" // small reusable snippet
  | "workflow" // a multi-step process the user follows
  | "person" // a name + context (collaborator, contact)
  | "project" // a named project + metadata
  | "credential_hint" // NOT the secret — just "uses 1Password for X"
  | "chat_summary" // distilled summary of a past conversation / session
  | "favorite_app" // a Composio toolkit / integration the user uses often
  | "other";

export const MEMORY_KINDS: readonly MemoryKind[] = [
  "directory",
  "command",
  "preference",
  "fact",
  "code_snippet",
  "workflow",
  "person",
  "project",
  "credential_hint",
  "chat_summary",
  "favorite_app",
  "other",
] as const;

export type MemoryEntry = {
  id: string;
  tenantId: string;
  kind: MemoryKind;
  // Short canonical title — what this entry is "about". 1-line.
  title: string;
  // The actual content. For directories this is the path; for commands the
  // command string; for facts/preferences the assertion text; for snippets
  // the code. Kept exactly as the user wrote it where possible.
  content: string;
  // Single-paragraph natural-language summary written by the enrichment LLM
  // at write-time. Used for retrieval display and prompt injection.
  summary?: string;
  // Open-ended tags from the enrichment LLM (lowercase, kebab-case). Used
  // for keyword retrieval and the tag index.
  labels: string[];
  // 0..1 — enrichment LLM's read of how reusable / load-bearing this memory
  // is. Higher importance gets injected more often.
  importance: number;
  // Type-specific structured fields. Kept as a loose record so we don't have
  // to migrate the schema every time a new field shows up.
  fields?: Record<string, unknown>;
  createdAt: number;
  lastAccessedAt: number;
  accessCount: number;
};

// --- env knobs --------------------------------------------------------------

export function memoryCapPerKind(): number {
  const n = Number(env("MEMORY_CAP_PER_KIND") ?? "200");
  return Number.isFinite(n) && n > 0 ? n : 200;
}
export function memoryDefaultRetrievalLimit(): number {
  const n = Number(env("MEMORY_RETRIEVAL_LIMIT") ?? "8");
  return Number.isFinite(n) && n > 0 ? Math.min(40, n) : 8;
}

// --- ids + keys -------------------------------------------------------------

function newId(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto?.randomUUID) {
    return "m_" + g.crypto.randomUUID().replace(/-/g, "").slice(0, 14);
  }
  return (
    "m_" +
    Date.now().toString(36) +
    Math.random().toString(36).slice(2, 10)
  );
}
function safeTag(t: string): string {
  return t
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
}
function entryKey(tid: string, id: string) {
  return `mem:${tid}:e:${id}`;
}
function idxRecent(tid: string) {
  return `mem:${tid}:idx:recent`;
}
function idxAll(tid: string) {
  return `mem:${tid}:idx:all`;
}
function idxKind(tid: string, kind: MemoryKind) {
  return `mem:${tid}:idx:kind:${kind}`;
}
function idxTag(tid: string, tag: string) {
  return `mem:${tid}:idx:tag:${tag}`;
}

// --- writes -----------------------------------------------------------------

export type PutMemoryInput = {
  tenantId: string;
  kind: MemoryKind;
  title: string;
  content: string;
  summary?: string;
  labels?: string[];
  importance?: number;
  fields?: Record<string, unknown>;
};

export async function putMemory(
  input: PutMemoryInput
): Promise<MemoryEntry> {
  const store = getStore();
  const now = Date.now();
  const id = newId();
  const labels = (input.labels ?? []).map(safeTag).filter(Boolean);
  const importance = Math.max(
    0,
    Math.min(1, input.importance ?? 0.5)
  );

  const entry: MemoryEntry = {
    id,
    tenantId: input.tenantId,
    kind: input.kind,
    title: input.title.slice(0, 200),
    content: input.content.slice(0, 8000),
    summary: input.summary?.slice(0, 800),
    labels,
    importance,
    fields: input.fields,
    createdAt: now,
    lastAccessedAt: now,
    accessCount: 0,
  };

  await store.set(entryKey(input.tenantId, id), entry);
  await store.zadd(idxRecent(input.tenantId), now, id);
  await store.zadd(idxAll(input.tenantId), now, id);
  await store.zadd(idxKind(input.tenantId, input.kind), now, id);
  for (const t of labels) {
    await store.sadd(idxTag(input.tenantId, t), id);
  }

  // Enforce per-kind cap. Eviction policy: oldest-lastAccessed wins the boot.
  await pruneKindIfOverCap(input.tenantId, input.kind);

  return entry;
}

async function pruneKindIfOverCap(
  tenantId: string,
  kind: MemoryKind
): Promise<void> {
  const cap = memoryCapPerKind();
  const store = getStore();
  // Approximate count via zrangebyscore(0, now). For accuracy we'd need ZCARD,
  // which our Store interface doesn't expose. Pull all ids in the kind index
  // and prune by count — fine because per-kind cap is small (default 200).
  const all = await store.zrangebyscore(
    idxKind(tenantId, kind),
    0,
    Number.MAX_SAFE_INTEGER
  );
  if (all.length <= cap) return;
  // Oldest first — the index uses lastAccessedAt as score, so zrangebyscore
  // sorts ascending. Drop the leading excess.
  const excess = all.slice(0, all.length - cap);
  for (const id of excess) {
    await deleteMemoryInternal(tenantId, id, kind);
  }
}

export async function touchMemory(
  tenantId: string,
  id: string
): Promise<MemoryEntry | null> {
  const store = getStore();
  const entry = await store.get<MemoryEntry>(entryKey(tenantId, id));
  if (!entry) return null;
  const now = Date.now();
  entry.lastAccessedAt = now;
  entry.accessCount += 1;
  await store.set(entryKey(tenantId, id), entry);
  await store.zadd(idxRecent(tenantId), now, id);
  await store.zadd(idxKind(tenantId, entry.kind), now, id);
  return entry;
}

export async function deleteMemory(
  tenantId: string,
  id: string
): Promise<boolean> {
  const store = getStore();
  const entry = await store.get<MemoryEntry>(entryKey(tenantId, id));
  if (!entry) return false;
  await deleteMemoryInternal(tenantId, id, entry.kind);
  for (const t of entry.labels) {
    await store.srem(idxTag(tenantId, t), id);
  }
  return true;
}

async function deleteMemoryInternal(
  tenantId: string,
  id: string,
  kind: MemoryKind
): Promise<void> {
  const store = getStore();
  await store.del(entryKey(tenantId, id));
  await store.zrem(idxRecent(tenantId), id);
  await store.zrem(idxAll(tenantId), id);
  await store.zrem(idxKind(tenantId, kind), id);
}

// --- reads ------------------------------------------------------------------

export async function getMemory(
  tenantId: string,
  id: string
): Promise<MemoryEntry | null> {
  const store = getStore();
  return (await store.get<MemoryEntry>(entryKey(tenantId, id))) ?? null;
}

async function hydrate(
  tenantId: string,
  ids: string[]
): Promise<MemoryEntry[]> {
  const store = getStore();
  const out: MemoryEntry[] = [];
  for (const id of ids) {
    const e = await store.get<MemoryEntry>(entryKey(tenantId, id));
    if (e) out.push(e);
  }
  return out;
}

export async function listRecent(
  tenantId: string,
  limit: number
): Promise<MemoryEntry[]> {
  const store = getStore();
  // zrangebyscore returns ascending; we want most-recent first. Read all
  // recent ids ascending then reverse + slice. Wastes a bit on large stores;
  // bounded by per-kind cap so it's fine.
  const ids = await store.zrangebyscore(
    idxRecent(tenantId),
    0,
    Number.MAX_SAFE_INTEGER
  );
  const slice = ids.slice(-Math.max(1, limit)).reverse();
  return hydrate(tenantId, slice);
}

export async function listByKind(
  tenantId: string,
  kind: MemoryKind,
  limit = 50
): Promise<MemoryEntry[]> {
  const store = getStore();
  const ids = await store.zrangebyscore(
    idxKind(tenantId, kind),
    0,
    Number.MAX_SAFE_INTEGER
  );
  const slice = ids.slice(-Math.max(1, limit)).reverse();
  return hydrate(tenantId, slice);
}

export async function listByTag(
  tenantId: string,
  tag: string,
  limit = 50
): Promise<MemoryEntry[]> {
  const store = getStore();
  const ids = await store.smembers(idxTag(tenantId, safeTag(tag)));
  // smembers is unordered; hydrate then sort by lastAccessedAt desc.
  const entries = await hydrate(tenantId, ids);
  entries.sort((a, b) => b.lastAccessedAt - a.lastAccessedAt);
  return entries.slice(0, Math.max(1, limit));
}

// Returns counts per kind. Useful for /memories overview.
export async function countByKind(
  tenantId: string
): Promise<Record<MemoryKind, number>> {
  const store = getStore();
  const out = {} as Record<MemoryKind, number>;
  for (const k of MEMORY_KINDS) {
    const ids = await store.zrangebyscore(
      idxKind(tenantId, k),
      0,
      Number.MAX_SAFE_INTEGER
    );
    out[k] = ids.length;
  }
  return out;
}
