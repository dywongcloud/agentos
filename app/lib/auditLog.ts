// app/lib/auditLog.ts
//
// Per-tenant AUDIT log — distinct from the activity/event log. The audit log
// only captures STATE CHANGES that an operator/auditor cares about:
//
//   - Composio: integration connected / disconnected / expired / revoked
//   - Composio: trigger subscribed / unsubscribed
//   - Settings: autopilot on/off, debug mode flip, pairing, gateway token
//   - Auth: browser login captured, login forgotten
//
// It is NOT the event stream. Trigger events firing, jobs dispatching, chat
// turns, tool calls — those live in activityLog. This stream is meant to
// answer "what changed on the account in the last N days?".
//
// Redis schema:
//   audit:{tid}:log         LIST   newest-first JSON entries, capped
//   audit:{tid}:snap:conns  STRING JSON snapshot of last-known Composio
//                                   connected-accounts (used for drift
//                                   detection on read)

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";

export type AuditKind =
  // Composio integration state
  | "integration.connected"
  | "integration.disconnected"
  | "integration.expired"
  | "integration.revoked"
  | "integration.refreshed"
  | "trigger.subscribed"
  | "trigger.unsubscribed"
  // Bot configuration
  | "settings.autopilot.on"
  | "settings.autopilot.off"
  | "settings.debug.on"
  | "settings.debug.off"
  | "settings.pairing"
  | "settings.gateway_token"
  // Browser session lifecycle
  | "browser.login_captured"
  | "browser.login_forgotten"
  // High-impact tool executions — recorded because they modify external
  // state (Composio action), local filesystem (VFS write/shell), or take
  // real action on the live web (browser). Read-only tools intentionally
  // skipped so the audit log stays signal-heavy.
  | "tool.composio_exec"
  | "tool.vfs_write"
  | "tool.vfs_shell"
  | "tool.browser"
  // /code project lifecycle — long-running coding sessions (claude/opencode
  // in the sandbox). These materialize files in the VFS and can push to
  // GitHub, so every dispatched/finished/failed turn is auditable.
  // `tool.code_progress` captures the per-phase transitions inside a turn
  // (preparing sandbox, cloning repo, engine running, …) so the Activity
  // panel reflects what /code status shows.
  | "tool.code_dispatch"
  | "tool.code_progress"
  | "tool.code_turn_done"
  | "tool.code_turn_failed"
  | "tool.code_push"
  // /job lifecycle — deep-research / orchestration jobs dispatched via
  // `/job <prompt>` (or `/deep`, `/extended`). Mirrors the /code shape
  // so the Activity panel surfaces job dispatches, state transitions,
  // completions, and failures alongside code activity.
  | "tool.job_dispatch"
  | "tool.job_progress"
  | "tool.job_done"
  | "tool.job_failed"
  // Generic catch-all for ad-hoc audit writes
  | "system";

export type AuditEntry = {
  id: string;
  ts: number;
  kind: AuditKind;
  summary: string;
  // Before/after snapshot fields are optional but encouraged for state changes
  // so the auditor can see exactly what flipped.
  before?: string;
  after?: string;
  // Free-form meta for slug / account id / trigger id / etc.
  meta?: Record<string, unknown>;
};

const MAX_DEFAULT = 1000;
function maxEntries(): number {
  const n = Number(env("AUDIT_LOG_CAP") ?? "");
  return Number.isFinite(n) && n > 0 ? n : MAX_DEFAULT;
}

function logKey(tid: string): string {
  return `audit:${tid}:log`;
}

function newId(): string {
  return (
    "u_" +
    Date.now().toString(36) +
    Math.random().toString(36).slice(2, 8)
  );
}

export async function recordAudit(
  tenantId: string,
  entry: Omit<AuditEntry, "id" | "ts"> & { ts?: number }
): Promise<void> {
  if (!tenantId) return;
  const e: AuditEntry = {
    id: newId(),
    ts: entry.ts ?? Date.now(),
    kind: entry.kind,
    summary: (entry.summary ?? "").slice(0, 400),
    before: entry.before,
    after: entry.after,
    meta: entry.meta,
  };
  try {
    const store = getStore();
    await store.lpush(logKey(tenantId), JSON.stringify(e));
    await store.ltrim(logKey(tenantId), 0, maxEntries() - 1);
  } catch {
    // best-effort
  }
}

export type ListAuditOptions = {
  limit?: number;
  kind?: AuditKind;
  sinceMs?: number;
};

export async function listAudit(
  tenantId: string,
  opts: ListAuditOptions = {}
): Promise<AuditEntry[]> {
  const limit = Math.max(1, Math.min(500, opts.limit ?? 100));
  const store = getStore();
  const pull = Math.min(maxEntries(), limit * 5);
  const raw = await store.lrange(logKey(tenantId), 0, pull - 1);
  const out: AuditEntry[] = [];
  for (const line of raw) {
    let e: AuditEntry;
    try {
      e = JSON.parse(line) as AuditEntry;
    } catch {
      continue;
    }
    if (opts.kind && e.kind !== opts.kind) continue;
    if (opts.sinceMs != null && e.ts < opts.sinceMs) continue;
    out.push(e);
    if (out.length >= limit) break;
  }
  return out;
}

// Composio integration drift detection was removed: every OAuth handshake
// produces transient INITIALIZING → EXPIRED churn and the resulting audit
// rows drowned out the actual signal (tool calls + VFS writes). If we ever
// want a "did a connection expire?" view back, surface it as a one-shot
// status panel rather than a stream entry.
