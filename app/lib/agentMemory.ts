// app/lib/agentMemory.ts
//
// Redis-native vector memory + knowledgebase for sub-agents and workforces.
//
// Every memory belongs to a scope:
//   { kind: "agent" }      — private to one sub-agent
//   { kind: "workforce" }  — shared by every agent on a team
//   { kind: "shared" }     — tenant-wide, shared across all agents/teams
//
// Records carry their embedding vector inline (OpenAI text-embedding-3-small,
// 1536 dims). Search embeds the query once, then cosine-ranks the candidate
// records from one or more scopes in-process — instant at our scale (hundreds
// of memories per scope) with zero extra infra. Knowledgebase docs are chunked
// and stored as kind:"kb" records in the same index, so retrieval is unified.
//
// Redis layout (no TTL):
//   mem:rec:{id}         JSON   MemoryRecord (text + vec + meta)
//   mem:idx:{scopeKey}   LIST   record ids, newest first (capped)
//
// This module is workflow-reachable: no Node builtins; everything goes through
// getStore() + the AI SDK's fetch-based embeddings.

import { embed, embedMany } from "ai";
import { openai } from "@ai-sdk/openai";

import { getStore } from "@/app/lib/store";

const EMBED_MODEL = "text-embedding-3-small";
const MAX_PER_SCOPE = 1000; // ring-buffer cap per scope index
const SEARCH_SCAN = 500; // most-recent records scanned per scope at search time
const MAX_EMBED_CHARS = 8000; // truncate very long inputs before embedding

export type MemoryScope =
  | { kind: "agent"; agentId: string }
  | { kind: "workforce"; workforceId: string }
  | { kind: "shared" };

export type MemoryKind = "note" | "takeaway" | "fact" | "kb" | "solution";

export type MemoryRecord = {
  id: string;
  tenantId: string;
  scopeKey: string;
  scopeKind: MemoryScope["kind"];
  kind: MemoryKind;
  text: string;
  source?: string; // for kb: the document/source label
  meta?: Record<string, unknown>;
  vec: number[];
  ts: number;
};

export type MemoryHit = { record: MemoryRecord; score: number };

// --- keys -------------------------------------------------------------------

export function scopeKeyFor(tenantId: string, scope: MemoryScope): string {
  switch (scope.kind) {
    case "agent":
      return `agent:${scope.agentId}`;
    case "workforce":
      return `wf:${scope.workforceId}`;
    case "shared":
      return `shared:${tenantId}`;
  }
}

const recKey = (id: string) => `mem:rec:${id}`;
const idxKey = (scopeKey: string) => `mem:idx:${scopeKey}`;

function newId(): string {
  return "mem_" + globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 16);
}

// --- embeddings -------------------------------------------------------------

function clip(text: string): string {
  const t = (text || "").trim();
  return t.length > MAX_EMBED_CHARS ? t.slice(0, MAX_EMBED_CHARS) : t;
}

export async function embedText(text: string): Promise<number[]> {
  const { embedding } = await embed({
    model: openai.embedding(EMBED_MODEL),
    value: clip(text),
  });
  return embedding;
}

async function embedTexts(texts: string[]): Promise<number[][]> {
  if (!texts.length) return [];
  const { embeddings } = await embedMany({
    model: openai.embedding(EMBED_MODEL),
    values: texts.map(clip),
  });
  return embeddings;
}

export function cosine(a: number[], b: number[]): number {
  let dot = 0;
  let na = 0;
  let nb = 0;
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    dot += a[i] * b[i];
    na += a[i] * a[i];
    nb += b[i] * b[i];
  }
  if (na === 0 || nb === 0) return 0;
  return dot / (Math.sqrt(na) * Math.sqrt(nb));
}

// --- write ------------------------------------------------------------------

async function persist(rec: MemoryRecord): Promise<MemoryRecord> {
  const store = getStore();
  await store.set(recKey(rec.id), rec);
  await store.lpush(idxKey(rec.scopeKey), rec.id);
  // Trim the index ring buffer; orphaned record blobs are harmless (no TTL
  // needed — they're tiny and fall out of every search scan).
  await store.ltrim(idxKey(rec.scopeKey), 0, MAX_PER_SCOPE - 1);
  return rec;
}

export async function addMemory(args: {
  tenantId: string;
  scope: MemoryScope;
  text: string;
  kind?: MemoryKind;
  source?: string;
  meta?: Record<string, unknown>;
}): Promise<MemoryRecord> {
  const vec = await embedText(args.text);
  const rec: MemoryRecord = {
    id: newId(),
    tenantId: args.tenantId,
    scopeKey: scopeKeyFor(args.tenantId, args.scope),
    scopeKind: args.scope.kind,
    kind: args.kind ?? "note",
    text: args.text.trim(),
    ...(args.source ? { source: args.source } : {}),
    ...(args.meta ? { meta: args.meta } : {}),
    vec,
    ts: Date.now(),
  };
  return persist(rec);
}

// Split a document into ~900-char chunks on paragraph/sentence boundaries so
// each knowledgebase entry embeds a coherent, retrievable unit.
export function chunkText(text: string, target = 900): string[] {
  const clean = (text || "").replace(/\r\n/g, "\n").trim();
  if (!clean) return [];
  const paras = clean.split(/\n{2,}/);
  const chunks: string[] = [];
  let buf = "";
  const flush = () => {
    const t = buf.trim();
    if (t) chunks.push(t);
    buf = "";
  };
  for (const para of paras) {
    if ((buf + "\n\n" + para).length <= target) {
      buf = buf ? `${buf}\n\n${para}` : para;
      continue;
    }
    flush();
    if (para.length <= target) {
      buf = para;
      continue;
    }
    // Paragraph alone exceeds target — split on sentence boundaries.
    const sentences = para.split(/(?<=[.!?])\s+/);
    for (const s of sentences) {
      if ((buf + " " + s).length <= target) {
        buf = buf ? `${buf} ${s}` : s;
      } else {
        flush();
        buf = s.length <= target ? s : s.slice(0, target);
      }
    }
  }
  flush();
  return chunks;
}

export async function addKnowledge(args: {
  tenantId: string;
  scope: MemoryScope;
  source: string;
  text: string;
}): Promise<{ chunks: number; ids: string[] }> {
  const chunks = chunkText(args.text);
  if (!chunks.length) return { chunks: 0, ids: [] };
  const vecs = await embedTexts(chunks);
  const ids: string[] = [];
  for (let i = 0; i < chunks.length; i++) {
    const rec: MemoryRecord = {
      id: newId(),
      tenantId: args.tenantId,
      scopeKey: scopeKeyFor(args.tenantId, args.scope),
      scopeKind: args.scope.kind,
      kind: "kb",
      text: chunks[i],
      source: args.source,
      meta: { chunk: i, of: chunks.length },
      vec: vecs[i],
      ts: Date.now() + i,
    };
    await persist(rec);
    ids.push(rec.id);
  }
  return { chunks: chunks.length, ids };
}

// --- read -------------------------------------------------------------------

async function loadScope(scopeKey: string, limit: number): Promise<MemoryRecord[]> {
  const store = getStore();
  const ids = await store.lrange(idxKey(scopeKey), 0, Math.max(0, limit - 1));
  if (!ids.length) return [];
  const recs = await Promise.all(ids.map((id) => store.get<MemoryRecord>(recKey(id))));
  return recs.filter((r): r is MemoryRecord => !!r);
}

export async function listMemory(args: {
  tenantId: string;
  scope: MemoryScope;
  limit?: number;
}): Promise<MemoryRecord[]> {
  return loadScope(scopeKeyFor(args.tenantId, args.scope), args.limit ?? 50);
}

export async function countMemory(args: {
  tenantId: string;
  scope: MemoryScope;
}): Promise<number> {
  return getStore().llen(idxKey(scopeKeyFor(args.tenantId, args.scope)));
}

// Cosine-rank records across one or more scopes against a free-text query.
export async function searchMemory(args: {
  tenantId: string;
  scopes: MemoryScope[];
  query: string;
  topK?: number;
  kinds?: MemoryKind[];
}): Promise<MemoryHit[]> {
  if (!args.query.trim() || !args.scopes.length) return [];
  const qvec = await embedText(args.query);
  const seen = new Set<string>();
  const pool: MemoryRecord[] = [];
  const batches = await Promise.all(
    args.scopes.map((s) => loadScope(scopeKeyFor(args.tenantId, s), SEARCH_SCAN))
  );
  for (const batch of batches) {
    for (const r of batch) {
      if (seen.has(r.id)) continue;
      if (args.kinds && !args.kinds.includes(r.kind)) continue;
      seen.add(r.id);
      pool.push(r);
    }
  }
  const hits = pool.map((record) => ({ record, score: cosine(qvec, record.vec) }));
  hits.sort((a, b) => b.score - a.score);
  return hits.slice(0, args.topK ?? 8);
}

export async function deleteMemory(id: string): Promise<void> {
  const store = getStore();
  const rec = await store.get<MemoryRecord>(recKey(id));
  await store.del(recKey(id));
  if (rec) await store.srem(idxKey(rec.scopeKey), id); // best-effort if set-backed
}

// Build a compact context block (for an agent's system prompt) from the most
// relevant shared + agent-scoped memories for a given task.
export async function recallContext(args: {
  tenantId: string;
  agentId?: string;
  workforceId?: string;
  query: string;
  topK?: number;
}): Promise<string> {
  const scopes: MemoryScope[] = [{ kind: "shared" }];
  if (args.workforceId) scopes.push({ kind: "workforce", workforceId: args.workforceId });
  if (args.agentId) scopes.push({ kind: "agent", agentId: args.agentId });
  const hits = await searchMemory({
    tenantId: args.tenantId,
    scopes,
    query: args.query,
    topK: args.topK ?? 6,
  });
  const useful = hits.filter((h) => h.score > 0.2);
  if (!useful.length) return "";
  const lines = useful.map((h) => {
    const tag =
      h.record.scopeKind === "agent"
        ? "you"
        : h.record.scopeKind === "workforce"
          ? "team"
          : "shared";
    const src = h.record.source ? ` (${h.record.source})` : "";
    return `- [${tag}${src}] ${h.record.text.replace(/\s+/g, " ").slice(0, 400)}`;
  });
  return `Relevant memory & knowledge:\n${lines.join("\n")}`;
}
