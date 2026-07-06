// app/lib/bulkRuns.ts
//
// Durable BULK RUN engine — large-data tasks like "email all 1000+ contacts in
// tab 2 of this spreadsheet, one personalized email per row".
//
// Why this exists: an agent turn (or even a deep job) is the WRONG shape for
// N≈1000 repetitions — LLM tool-calling per item blows past context/tool-step
// caps, costs a fortune, and a single serverless timeout loses everything.
// A bulk run inverts it: the LLM sets up the run ONCE (which read fetches the
// rows, which action runs per row, how row columns map into the action's
// args), then a durable workflow executes deterministically:
//
//   - fetch step: one Composio READ (e.g. GOOGLESHEETS_BATCH_GET of the tab),
//     rows normalized to header-keyed objects and persisted
//   - batch steps: small slices (default 5 items) so every step finishes well
//     inside serverless limits; the workflow checkpoints between batches and
//     survives crashes/restarts
//   - per-item retries with exponential backoff (default 3 attempts)
//   - per-item idempotency ledger: an item that already succeeded is NEVER
//     re-executed, even when a batch step is retried/replayed — no double-sent
//     emails
//   - rate limiting: inter-item and inter-batch delays
//   - progress pings to the chat, /stop-aware cancellation, final summary
//
// Zero LLM calls inside the loop — templates are pure string substitution.

import { getStore } from "@/app/lib/store";
import type { Channel } from "@/app/lib/identity";

// --- types --------------------------------------------------------------

export type BulkFetchSpec = {
  tool: string; // Composio READ slug, e.g. GOOGLESHEETS_BATCH_GET
  args: Record<string, unknown>;
  // Dot-path to the array of rows in the response; when absent we deep-search
  // for the largest array. Array-of-arrays responses (sheets) are converted to
  // header-keyed objects when headerRow (default true for array-of-arrays).
  itemsPath?: string;
  headerRow?: boolean;
};

export type BulkActionSpec = {
  tool: string; // Composio ACTION slug, e.g. GMAIL_SEND_EMAIL
  // Arg templates: string values may reference row fields as {{Column Name}}
  // (case-insensitive header match). Resolved per row with plain substitution.
  argsTemplate: Record<string, string>;
};

export type BulkRunStatus = "pending" | "running" | "done" | "failed" | "cancelled";

export type BulkRun = {
  id: string; // "bulk_xxxx"
  tenantId: string;
  channel: Channel;
  sessionId: string;
  description: string;
  fetch: BulkFetchSpec;
  action: BulkActionSpec;
  // Safety/behavior knobs (defaults applied at create time).
  maxItems: number;
  batchSize: number;
  itemDelayMs: number;
  dryRun: boolean;
  status: BulkRunStatus;
  total: number; // set after fetch
  done: number; // succeeded
  failed: number;
  skipped: number; // empty rows / missing required fields
  error?: string;
  createdAt: number;
  finishedAt?: number;
};

// --- keys ---------------------------------------------------------------

const runKey = (id: string) => `bulk:run:${id}`;
const itemsKey = (id: string) => `bulk:items:${id}`;
const statusKey = (id: string) => `bulk:status:${id}`; // hash idx -> ok|skip|fail:<reason>
const failuresKey = (id: string) => `bulk:failures:${id}`; // list of "idx: reason"
const failedIdxKey = (id: string) => `bulk:failidx:${id}`; // list of failed idx (for retry)
const byTenantKey = (t: string) => `bulk:by_tenant:${t}`;

function newBulkId(): string {
  return "bulk_" + globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 12);
}

// --- CRUD ---------------------------------------------------------------

export async function createBulkRun(args: {
  tenantId: string;
  channel: Channel;
  sessionId: string;
  description: string;
  fetch: BulkFetchSpec;
  action: BulkActionSpec;
  maxItems?: number;
  batchSize?: number;
  itemDelayMs?: number;
  dryRun?: boolean;
}): Promise<BulkRun> {
  const run: BulkRun = {
    id: newBulkId(),
    tenantId: args.tenantId,
    channel: args.channel,
    sessionId: args.sessionId,
    description: args.description.slice(0, 300),
    fetch: args.fetch,
    action: args.action,
    maxItems: Math.min(Math.max(1, args.maxItems ?? 2000), 5000),
    batchSize: Math.min(Math.max(1, args.batchSize ?? 5), 25),
    // Capped so batchSize×delay (+retries) always keeps one batch step well
    // inside serverless execution limits.
    itemDelayMs: Math.min(Math.max(0, args.itemDelayMs ?? 700), 5_000),
    dryRun: args.dryRun ?? false,
    status: "pending",
    total: 0,
    done: 0,
    failed: 0,
    skipped: 0,
    createdAt: Date.now(),
  };
  const store = getStore();
  await store.set(runKey(run.id), run);
  await store.sadd(byTenantKey(run.tenantId), run.id);
  return run;
}

export async function getBulkRun(id: string): Promise<BulkRun | null> {
  return getStore().get<BulkRun>(runKey(id));
}

export async function patchBulkRun(
  id: string,
  patch: Partial<BulkRun>
): Promise<BulkRun | null> {
  const cur = await getBulkRun(id);
  if (!cur) return null;
  const next = { ...cur, ...patch };
  await getStore().set(runKey(id), next);
  return next;
}

// --- items --------------------------------------------------------------

export type BulkItem = Record<string, string>;

export async function saveBulkItems(id: string, items: BulkItem[]): Promise<void> {
  await getStore().set(itemsKey(id), items);
}

export async function loadBulkItems(id: string): Promise<BulkItem[]> {
  return (await getStore().get<BulkItem[]>(itemsKey(id))) ?? [];
}

// Per-item idempotency ledger. "ok" means the side effect ALREADY HAPPENED —
// replayed/retried batches must skip it (this is what makes email sends safe
// under WDK step retries).
export async function getItemStatus(id: string, idx: number): Promise<string | null> {
  return getStore().hget<string>(statusKey(id), String(idx));
}

export async function setItemStatus(
  id: string,
  idx: number,
  status: string
): Promise<void> {
  await getStore().hset(statusKey(id), String(idx), status);
}

export async function recordBulkFailure(
  id: string,
  idx: number,
  reason: string
): Promise<void> {
  const store = getStore();
  await store.lpush(failuresKey(id), `row ${idx + 1}: ${reason.slice(0, 200)}`);
  await store.ltrim(failuresKey(id), 0, 49); // keep the 50 most recent
  await store.lpush(failedIdxKey(id), String(idx)); // full list — drives retry
}

export async function listBulkFailures(id: string, limit = 10): Promise<string[]> {
  return getStore().lrange(failuresKey(id), 0, Math.max(0, limit - 1));
}

// Prepare a finished run for a failed-rows-only retry: clear the "fail:" marks
// from the idempotency ledger (succeeded/skipped rows keep theirs, so they are
// never repeated), reset the failure counters, and set the run back to running.
// Caller then re-launches bulkWorkflow(runId). Returns the retryable count.
export async function resetBulkFailuresForRetry(id: string): Promise<number> {
  const store = getStore();
  const run = await getBulkRun(id);
  if (!run) return 0;
  const idxs = await store.lrange(failedIdxKey(id), 0, -1);
  const unique = [...new Set(idxs)];
  for (const idx of unique) {
    await store.hdel(statusKey(id), idx);
  }
  await store.del(failedIdxKey(id));
  await store.del(failuresKey(id));
  await patchBulkRun(id, {
    status: "running",
    failed: 0,
    error: undefined,
    finishedAt: undefined,
  });
  return unique.length;
}

// --- row normalization ----------------------------------------------------

function coerceJson(v: unknown): unknown {
  if (typeof v !== "string") return v;
  const t = v.trim();
  if (t.length > 1 && (t[0] === "{" || t[0] === "[")) {
    try {
      return JSON.parse(t);
    } catch {
      return v;
    }
  }
  return v;
}

function getByPath(obj: unknown, path: string): unknown {
  let cur: unknown = coerceJson(obj);
  for (const seg of path.split(".")) {
    if (cur == null || typeof cur !== "object") return undefined;
    cur = coerceJson((cur as Record<string, unknown>)[seg]);
  }
  return cur;
}

// Deep-search for the largest array — rows can hide at different depths across
// Composio response shapes (values, valueRanges[0].values, data.rows, …).
function deepFindLargestArray(obj: unknown, depth = 0): unknown[] {
  let best: unknown[] = [];
  if (depth > 6 || obj == null) return best;
  const o = coerceJson(obj);
  if (Array.isArray(o)) {
    if (o.length > best.length) best = o;
    for (const x of o) {
      const inner = deepFindLargestArray(x, depth + 1);
      if (inner.length > best.length) best = inner;
    }
    return best;
  }
  if (typeof o === "object") {
    for (const k of Object.keys(o as Record<string, unknown>)) {
      const inner = deepFindLargestArray((o as Record<string, unknown>)[k], depth + 1);
      if (inner.length > best.length) best = inner;
    }
  }
  return best;
}

// Normalize a fetch response into header-keyed row objects.
//   - array of arrays (sheets): first row = headers (unless headerRow false,
//     then columns become col1..colN)
//   - array of objects: stringify each value
export function normalizeRows(
  data: unknown,
  spec: { itemsPath?: string; headerRow?: boolean }
): BulkItem[] {
  const raw = spec.itemsPath ? getByPath(data, spec.itemsPath) : undefined;
  const arr = Array.isArray(raw) && raw.length ? raw : deepFindLargestArray(data);
  if (!arr.length) return [];

  if (Array.isArray(arr[0])) {
    const useHeader = spec.headerRow !== false;
    const headers: string[] = useHeader
      ? (arr[0] as unknown[]).map((h, i) => String(h ?? `col${i + 1}`).trim() || `col${i + 1}`)
      : (arr[0] as unknown[]).map((_x, i) => `col${i + 1}`);
    const body = useHeader ? arr.slice(1) : arr;
    return body.map((r) => {
      const row: BulkItem = {};
      const cells = Array.isArray(r) ? (r as unknown[]) : [r];
      headers.forEach((h, i) => {
        row[h] = cells[i] == null ? "" : String(cells[i]);
      });
      return row;
    });
  }

  return arr
    .filter((r) => r && typeof r === "object")
    .map((r) => {
      const row: BulkItem = {};
      for (const [k, v] of Object.entries(r as Record<string, unknown>)) {
        row[k] = v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v);
      }
      return row;
    });
}

// --- template resolution ----------------------------------------------------

// Replace {{Column Name}} tokens with the row's value for that column
// (case-insensitive, trimmed header match; also accepts a leading "col:").
// Returns { value, missing } so callers can skip rows lacking required fields.
export function resolveRowTemplate(
  tpl: string,
  row: BulkItem
): { value: string; missing: string[] } {
  const lower = new Map(Object.keys(row).map((k) => [k.trim().toLowerCase(), k]));
  const missing: string[] = [];
  const value = tpl.replace(/\{\{\s*(?:col:)?([^{}]+?)\s*\}\}/g, (_m, name: string) => {
    const key = lower.get(String(name).trim().toLowerCase());
    if (key === undefined) {
      missing.push(name.trim());
      return "";
    }
    return row[key] ?? "";
  });
  return { value, missing };
}
