// app/api/ui/audit/route.ts
//
// Activity feed for the dashboard. Merges two per-tenant sources into one
// time-sorted stream:
//   - activityLog (`activity:{tid}:log`) — the real event stream: tool calls,
//     job/code lifecycle, memory writes, and TRIGGER FIRES (e.g. a monday
//     board change that got AI-explained and delivered). This is where the
//     user's "what happened" lives.
//   - auditLog — state-change records: integration connect/disconnect,
//     trigger sub/unsub, settings flips, high-impact tool executions.
//
// Integration OAuth drift (the continuous INITIALIZING/EXPIRED/refreshed
// churn from Composio) is suppressed — it was producing a spam stream — but
// everything the agent actually DID is surfaced. Both logs share the
// AuditItem shape {id, ts, kind, summary, before?, after?, meta?}, so the
// merge is a straight concat + dedupe + sort.

import { NextResponse } from "next/server";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { listAudit, type AuditEntry } from "@/app/lib/auditLog";
import { listActivity, type ActivityEntry } from "@/app/lib/activityLog";
import { resolveUiTenant } from "@/app/lib/uiTenant";

type FeedItem = {
  id: string;
  ts: number;
  kind: string;
  summary: string;
  before?: string;
  after?: string;
  meta?: Record<string, unknown>;
};

// auditLog kinds that are pure OAuth churn — suppressed from the feed.
function isNoise(kind: string): boolean {
  if (kind === "integration.expired") return true;
  if (kind === "integration.revoked") return true;
  if (kind === "integration.refreshed") return true;
  if (kind === "integration.initializing") return true;
  if (kind === "browser.login_captured") return true;
  if (kind === "browser.login_forgotten") return true;
  return false;
}

export async function GET(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) {
    return NextResponse.json({ ok: true, tenant: null, items: [] });
  }
  const limit = Math.max(
    1,
    Math.min(200, Number(url.searchParams.get("limit") ?? "60"))
  );

  // Pull a wider window from both logs than `limit` so the merged result
  // still has enough rows after the noisy kinds get dropped. Both stores are
  // capped, so this is bounded.
  const window = Math.min(500, limit * 8);
  const [auditRaw, activityRaw] = await Promise.all([
    listAudit(tenant, { limit: window }).catch(() => [] as AuditEntry[]),
    listActivity(tenant, { limit: window }).catch(() => [] as ActivityEntry[]),
  ]);

  const merged: FeedItem[] = [];
  // activityLog: the real event stream — keep everything (its kinds are all
  // meaningful: tool/job/command/memory/trigger/login/code/browse/automation/
  // system). This is the spine of the feed.
  for (const a of activityRaw) {
    merged.push({
      id: a.id,
      ts: a.ts,
      kind: a.kind,
      summary: a.summary,
      meta: a.meta,
    });
  }
  // auditLog: state changes — keep the signal, drop the OAuth churn.
  for (const e of auditRaw) {
    if (isNoise(e.kind)) continue;
    merged.push({
      id: e.id,
      ts: e.ts,
      kind: e.kind,
      summary: e.summary,
      before: e.before,
      after: e.after,
      meta: e.meta,
    });
  }

  // Dedupe: same event can land in both logs (e.g. a composio exec). Collapse
  // by id first, then by a content signature (kind + summary + ts-to-second)
  // so near-duplicate rows from the two sources don't double up.
  const seen = new Set<string>();
  const deduped: FeedItem[] = [];
  for (const it of merged.sort((a, b) => b.ts - a.ts)) {
    const sig = `${it.kind}|${it.summary}|${Math.round(it.ts / 1000)}`;
    if (seen.has(it.id) || seen.has(sig)) continue;
    seen.add(it.id);
    seen.add(sig);
    deduped.push(it);
  }

  const items = deduped.slice(0, limit);
  return NextResponse.json({ ok: true, tenant, count: items.length, items });
}
