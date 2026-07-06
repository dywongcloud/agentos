// app/lib/activityLog.ts
//
// Per-tenant activity log. Append-only, capped, Redis-backed. Used so the
// user (or the agent on the user's behalf) can answer "what have I been
// doing?" / "what jobs have run?" / "what did I subscribe to?" without
// having to dig through Vercel logs.
//
// Idempotency / determinism: same query, same result. No LLM in the hot
// path. Like the memory store, gpt-4o (or any LLM) only ever runs at
// write-time if the caller chooses to enrich a summary — most callsites
// just write a fixed string.
//
// Redis schema:
//   activity:{tid}:log    LIST   newest-first; capped at MAX_ENTRIES

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";

export type ActivityKind =
  | "tool" // a tool the agent called
  | "job" // a /job dispatch / lifecycle event
  | "command" // a Telegram slash command
  | "memory" // memory writes (remember/forget)
  | "trigger" // composio trigger sub/unsub or event fired
  | "login" // browser session captured
  | "code" // /code (claude code) invocation
  | "browse" // browse_web call
  | "automation" // user-defined automation ("flow") run
  | "system"; // anything else

export type ActivityEntry = {
  id: string;
  ts: number;
  kind: ActivityKind;
  summary: string;
  meta?: Record<string, unknown>;
};

const MAX_ENTRIES_DEFAULT = 500;
function maxEntries(): number {
  const n = Number(env("ACTIVITY_LOG_CAP") ?? "");
  return Number.isFinite(n) && n > 0 ? n : MAX_ENTRIES_DEFAULT;
}

function logKey(tenantId: string): string {
  return `activity:${tenantId}:log`;
}

function newId(): string {
  return (
    "a_" +
    Date.now().toString(36) +
    Math.random().toString(36).slice(2, 8)
  );
}

export async function recordActivity(
  tenantId: string,
  entry: Omit<ActivityEntry, "id" | "ts"> & { ts?: number }
): Promise<void> {
  if (!tenantId) return;
  const store = getStore();
  const e: ActivityEntry = {
    id: newId(),
    ts: entry.ts ?? Date.now(),
    kind: entry.kind,
    summary: (entry.summary ?? "").slice(0, 400),
    meta: entry.meta,
  };
  try {
    await store.lpush(logKey(tenantId), JSON.stringify(e));
    await store.ltrim(logKey(tenantId), 0, maxEntries() - 1);
  } catch {
    // Activity logging is best-effort — never block a real operation on it.
  }
}

export type ListActivityOptions = {
  limit?: number;
  kind?: ActivityKind;
  sinceMs?: number;
  searchSubstring?: string;
};

export async function listActivity(
  tenantId: string,
  opts: ListActivityOptions = {}
): Promise<ActivityEntry[]> {
  const limit = Math.max(1, Math.min(200, opts.limit ?? 50));
  const store = getStore();
  // Read more than `limit` so we can filter and still hit the target count.
  const pull = Math.min(maxEntries(), limit * 5);
  const raw = await store.lrange(logKey(tenantId), 0, pull - 1);
  const out: ActivityEntry[] = [];
  for (const line of raw) {
    let e: ActivityEntry;
    try {
      e = JSON.parse(line) as ActivityEntry;
    } catch {
      continue;
    }
    if (opts.kind && e.kind !== opts.kind) continue;
    if (opts.sinceMs != null && e.ts < opts.sinceMs) continue;
    if (
      opts.searchSubstring &&
      !e.summary.toLowerCase().includes(opts.searchSubstring.toLowerCase())
    )
      continue;
    out.push(e);
    if (out.length >= limit) break;
  }
  return out;
}

export async function countActivity(tenantId: string): Promise<number> {
  const store = getStore();
  try {
    return await store.llen(logKey(tenantId));
  } catch {
    return 0;
  }
}
