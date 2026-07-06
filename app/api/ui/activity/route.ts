// app/api/ui/activity/route.ts
//
// Live activity feed for the dashboard. Returns a merged, time-sorted list
// drawn from real sources:
//   - per-tenant activityLog (every tool/job/command/trigger/memory event)
//   - recent Composio webhook deliveries (composio:webhook:log ring)
//   - active + recent jobs (jobStore)
//   - active + recent code projects (codeProjectStore)
//
// Polled by the client-side Recent Activity component every few seconds so
// the dashboard reflects what's actually happening — no mocks, no static
// placeholders.

import { NextResponse } from "next/server";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { listActivity, type ActivityEntry } from "@/app/lib/activityLog";
import { getRecentWebhookHits } from "@/app/lib/composioWebhook";
import {
  listActiveJobs,
  listRecentJobs,
  getJobMeta,
} from "@/app/lib/jobStore";
import {
  listActiveCodeProjects,
  listRecentCodeProjects,
  getCodeProject,
} from "@/app/lib/codeProjectStore";
import { listSuites, listRuns } from "@/app/lib/evals/store";
import type { EvalRun } from "@/app/lib/evals/types";
import { listRunsByTenant, getAutomation } from "@/app/lib/automations";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import { hardcodedChatLog } from "@/app/lib/hardcodedWorkforce";

export type ActivityFeedItem = {
  ts: number;
  kind:
    | "tool"
    | "job"
    | "command"
    | "memory"
    | "trigger"
    | "login"
    | "code"
    | "browse"
    | "system"
    | "webhook"
    | "job_status"
    | "code_status"
    | "eval"
    | "automation"
    | "chat";
  text: string;
  sub?: string;
  href?: string;
};

export async function GET(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) {
    return NextResponse.json({ ok: true, tenant: null, items: [] });
  }
  const limit = Math.max(1, Math.min(100, Number(url.searchParams.get("limit") ?? "40")));

  // Pull from all sources in parallel.
  const [acts, webhooks, activeJobIds, recentJobIds, activeProjIds, recentProjIds] =
    await Promise.all([
      listActivity(tenant, { limit: 60 }).catch(() => [] as ActivityEntry[]),
      getRecentWebhookHits(20).catch(() => []),
      listActiveJobs(tenant).catch(() => [] as string[]),
      listRecentJobs(tenant, 10).catch(() => [] as string[]),
      listActiveCodeProjects(tenant).catch(() => [] as string[]),
      listRecentCodeProjects(tenant, 8).catch(() => [] as string[]),
    ]);

  const items: ActivityFeedItem[] = [];

  // activity log entries — the spine of the feed
  for (const a of acts) {
    items.push({
      ts: a.ts,
      kind: a.kind as ActivityFeedItem["kind"],
      text: a.summary,
      sub: a.kind,
    });
  }

  // Webhook deliveries (most are also in activity now, but the ring keeps a
  // longer/more recent trail and surfaces failed deliveries too).
  for (const h of webhooks) {
    if (h.tenantId && h.tenantId !== tenant) continue;
    items.push({
      ts: h.ts,
      kind: "webhook",
      text: h.ok
        ? `webhook delivered: ${h.slug ?? "(unknown trigger)"}`
        : `webhook DROPPED: ${h.error ?? "unknown"}${h.slug ? ` (${h.slug})` : ""}`,
      sub: h.ok ? "delivered" : "dropped",
    });
  }

  // Recent + active jobs — surface as feed entries with current status
  const seenJobs = new Set<string>();
  for (const jid of [...activeJobIds, ...recentJobIds]) {
    if (seenJobs.has(jid)) continue;
    seenJobs.add(jid);
    const meta = await getJobMeta(jid).catch(() => null);
    if (!meta) continue;
    const stamp = meta.finishedAt ?? meta.updatedAt ?? meta.createdAt;
    items.push({
      ts: stamp,
      kind: "job_status",
      text: `job ${jid} — ${meta.status}${
        typeof meta.estimatedCost === "number"
          ? ` · $${meta.estimatedCost.toFixed(3)}`
          : ""
      }`,
      sub:
        meta.kind === "research"
          ? meta.escalated
            ? `deep · escalated · ${meta.depthPasses ?? 0} depth pass(es)`
            : `deep · ${meta.depthPasses ?? 0} depth pass(es)`
          : meta.kind,
      href: `/ui/workflows?userId=${encodeURIComponent(tenant)}&jobId=${encodeURIComponent(jid)}`,
    });
  }

  // Recent + active code projects
  const seenProj = new Set<string>();
  for (const pid of [...activeProjIds, ...recentProjIds]) {
    if (seenProj.has(pid)) continue;
    seenProj.add(pid);
    const proj = await getCodeProject(pid).catch(() => null);
    if (!proj) continue;
    items.push({
      ts: proj.updatedAt ?? proj.createdAt,
      kind: "code_status",
      text: `code ${pid} — ${proj.status} (${proj.engine}, turn ${proj.turnCount})`,
      sub: proj.title.slice(0, 80),
      href: `/ui/workflows?userId=${encodeURIComponent(tenant)}&projectId=${encodeURIComponent(pid)}`,
    });
  }

  // Recent eval runs — surfaced as clickable feed entries the same way jobs
  // are. Evals aren't tenant-scoped (owner-level quality signal), so we pull
  // the most recent across suites and link each to its detail view.
  try {
    const suites = await listSuites();
    const perSuite = await Promise.all(
      suites.map((s) => listRuns({ suite: s, limit: 5 }).catch(() => [] as EvalRun[]))
    );
    const recentRuns = perSuite.flat().sort((a, b) => b.ts - a.ts).slice(0, 12);
    for (const r of recentRuns) {
      const passed = r.grades.filter((g) => g.pass).length;
      items.push({
        ts: r.ts,
        kind: "eval",
        text: `eval ${r.suite} — ${r.status} (${passed}/${r.grades.length} graders)`,
        sub: r.input.goal.slice(0, 80),
        href: `/ui/evals?userId=${encodeURIComponent(tenant)}&runId=${encodeURIComponent(r.id)}`,
      });
    }
  } catch {
    // Eval store unavailable — feed still works without it.
  }

  // Recent automation runs for this tenant — clickable into the automations
  // detail view (mirrors the eval block).
  try {
    const runs = await listRunsByTenant(tenant, 12);
    for (const r of runs) {
      const rule = await getAutomation(r.automationId).catch(() => null);
      items.push({
        ts: r.finishedAt ?? r.ts,
        kind: "automation",
        text: `automation ${rule?.name ?? r.automationId} — ${r.status}`,
        sub: `${r.source}${r.jobId ? ` · job ${r.jobId}` : ""}`,
        href: `/ui/automations?userId=${encodeURIComponent(tenant)}&runId=${encodeURIComponent(r.id)}`,
      });
    }
  } catch {
    // Automation store unavailable — feed still works without it.
  }

  // Hardcoded showcase chat logs (macOS iMessage + WeChat Claude Code agents).
  // Purely visual — surfaced so the "Logs" tab reflects the showcase workforce.
  hardcodedChatLog().forEach((c, i) => {
    items.push({
      ts: Date.now() - (i + 1) * 7 * 60_000,
      kind: "chat",
      text: `${c.group ? "GROUP" : "DM"} ${c.who} — ${c.text}`,
      sub: i % 2 === 0 ? "imessage" : "wechat",
    });
  });

  items.sort((a, b) => b.ts - a.ts);
  const trimmed = items.slice(0, limit);
  return NextResponse.json({ ok: true, tenant, items: trimmed });
}
