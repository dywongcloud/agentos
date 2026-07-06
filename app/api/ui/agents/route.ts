// app/api/ui/agents/route.ts
//
// Data feed for the /ui/agents workforce canvas. Tenant-scoped, same auth as
// the rest of the /api/ui surface. Ops:
//   ?op=list               → { agents, teams (with trigger/status from the
//                             backing automation rule), bots (no tokens) }
//   ?op=runs&teamId=<id>   → recent automation runs for that team's rule
//   ?op=run&runId=<id>     → one run: status + per-stage records (member
//                             outputs, picked agents, linked job ids)
//   ?op=activity&teamId=<id>[&range=24h|week]
//                           → real task-activity feed for the team's member
//                             agents: tasks, bucket counts, timeline histogram
//   ?op=triggers[&toolkit=&keyword=] → trigger catalog for the builder:
//                             Composio native trigger types merged with our
//                             custom polling types (monday.com etc.)
//   POST {op:"create_workforce", description, trigger?, triggerConfig?}
//                           → compile + persist a new workforce from the UI

import { NextResponse } from "next/server";

import { env } from "@/app/lib/env";
import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import type { Channel } from "@/app/lib/identity";
import type { CompiledTrigger } from "@/app/steps/compileAutomationStep";
import { createWorkforceFromSpec } from "@/app/lib/workforceService";
import { getSessionMeta, getLastSession } from "@/app/lib/sessionMeta";
import {
  listTriggerTypes,
  listConnectedToolkits,
} from "@/app/lib/composioConnections";
import { listCustomTriggerTypes } from "@/app/lib/customTriggers";
import {
  listAgentsByTenant,
  listWorkforcesByTenant,
  listAgentBotsByTenant,
  getWorkforce,
  getStageRecords,
  getSubAgent,
  getAgentsByIds,
  scopeForAgent,
  putSubAgent,
} from "@/app/lib/agents";
import { MODEL_CATALOG, isKnownModel } from "@/app/lib/modelCatalog";
import {
  addMemory,
  addKnowledge,
  searchMemory,
  listMemory,
  countMemory,
  type MemoryScope,
  type MemoryRecord,
} from "@/app/lib/agentMemory";
import { sharedVfsList } from "@/app/lib/sharedVfs";
import {
  HC_TEAM_ID,
  hardcodedSubAgents,
  hardcodedWorkforce,
} from "@/app/lib/hardcodedWorkforce";
import { agentTurn } from "@/app/steps/agentTurn";
import { resolveModelName } from "@/app/lib/modelRouting";
import {
  loadHistoryStep,
  saveHistoryStep,
} from "@/app/steps/sessionStateSteps";
import type { ModelMessage } from "ai";
import {
  getAutomation,
  getRun,
  listRunsByRule,
  type AutomationTrigger,
} from "@/app/lib/automations";
import {
  listRecentJobs,
  getJobMeta,
  type JobMeta,
} from "@/app/lib/jobStore";

export const dynamic = "force-dynamic";

function triggerLabel(t: AutomationTrigger | null): string {
  if (!t) return "manual";
  switch (t.kind) {
    case "schedule":
      return t.cron
        ? `cron ${t.cron}${t.tz ? ` (${t.tz})` : ""}`
        : `every ${Math.round((t.everyMs ?? 0) / 1000)}s`;
    case "composio":
      return t.triggerType;
    case "webhook":
      return "webhook";
    case "chat":
      return `chat /${t.pattern}/${t.flags ?? "i"}`;
  }
}

type ActivityStatus =
  | "Error"
  | "Escalated"
  | "To review"
  | "Complete"
  | "Running";

// Map a job's raw status + escalation flag to the dashboard's coarse buckets.
function activityStatus(m: JobMeta): ActivityStatus {
  if (m.status === "failed" || m.status === "cancelled") return "Error";
  if (m.escalated) return "Escalated";
  if (m.status === "needs_input" || m.status === "clarifying") return "To review";
  if (m.status === "done") return "Complete";
  return "Running";
}

function memOut(r: MemoryRecord) {
  return {
    id: r.id,
    kind: r.kind,
    scope: r.scopeKind,
    text: r.text.length > 280 ? r.text.slice(0, 277) + "…" : r.text,
    source: r.source ?? null,
    ts: r.ts,
  };
}

// Resolve a scope request to a MemoryScope, validating tenant ownership for
// agent/workforce scopes. Returns null on a bad/cross-tenant reference.
async function resolveScope(
  tenant: string,
  scope: string | undefined,
  agentId: string | undefined,
  workforceId: string | undefined
): Promise<MemoryScope | null> {
  if (scope === "agent") {
    if (!agentId) return null;
    const a = await getSubAgent(agentId);
    if (!a || a.tenantId !== tenant) return null;
    return { kind: "agent", agentId };
  }
  if (scope === "workforce") {
    if (!workforceId) return null;
    const t = await getWorkforce(workforceId);
    if (!t || t.tenantId !== tenant) return null;
    return { kind: "workforce", workforceId };
  }
  return { kind: "shared" };
}

export async function GET(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) {
    return NextResponse.json({ error: "unknown tenant" }, { status: 400 });
  }

  const op = url.searchParams.get("op") ?? "list";

  if (op === "list") {
    const [agents, teams, bots] = await Promise.all([
      listAgentsByTenant(tenant),
      listWorkforcesByTenant(tenant),
      listAgentBotsByTenant(tenant),
    ]);

    const teamsOut = await Promise.all(
      teams.map(async (t) => {
        const rule = t.automationId ? await getAutomation(t.automationId) : null;
        return {
          id: t.id,
          name: t.name,
          emoji: t.emoji ?? null,
          spec: t.spec,
          stages: t.stages,
          automationId: t.automationId,
          enabled: t.enabled,
          createdAt: t.createdAt,
          lastRunId: t.lastRunId ?? null,
          trigger: rule?.trigger ?? null,
          triggerLabel: triggerLabel(rule?.trigger ?? null),
          status: rule?.status ?? null,
        };
      })
    );

    return NextResponse.json({
      agents: agents.map((a) => ({
        id: a.id,
        name: a.name,
        emoji: a.emoji,
        persona: a.persona,
        toolkits: a.toolkits,
        skills: a.skills ?? null,
        telegramBotId: a.telegramBotId ?? null,
        modelName: a.modelName ?? null,
        createdAt: a.createdAt,
      })),
      teams: teamsOut,
      bots: bots.map((b) => ({
        botId: b.botId,
        agentId: b.agentId,
        username: b.username,
      })),
    });
  }

  if (op === "models") {
    // Catalog for the per-agent model picker. defaultModel is what an agent
    // runs on when it has no explicit override (the env-driven "smart" model).
    return NextResponse.json({
      models: MODEL_CATALOG,
      defaultModel: resolveModelName("smart"),
    });
  }

  if (op === "runs") {
    const teamId = url.searchParams.get("teamId") ?? "";
    const team = await getWorkforce(teamId);
    if (!team || team.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown team" }, { status: 404 });
    }
    const runs = team.automationId ? await listRunsByRule(team.automationId, 20) : [];
    return NextResponse.json({
      runs: runs.map((r) => ({
        id: r.id,
        status: r.status,
        source: r.source,
        startedAt: r.ts,
        finishedAt: r.finishedAt ?? null,
        resultText: r.resultText ?? null,
      })),
    });
  }

  if (op === "run") {
    const runId = url.searchParams.get("runId") ?? "";
    const run = await getRun(runId);
    if (!run || run.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown run" }, { status: 404 });
    }
    const stages = await getStageRecords(runId);
    return NextResponse.json({
      run: {
        id: run.id,
        status: run.status,
        source: run.source,
        startedAt: run.ts,
        finishedAt: run.finishedAt ?? null,
        resultText: run.resultText ?? null,
      },
      stages,
    });
  }

  // Office snapshot: everything the per-workforce "workplace" view needs in one
  // round-trip — the team, its agents (with per-agent memory counts), recent
  // team + shared memories, and the shared VFS file listing.
  if (op === "office") {
    const teamId = url.searchParams.get("teamId") ?? "";
    // Showcase team is purely visual and not in Redis — synthesize its office.
    if (teamId === HC_TEAM_ID) {
      const hcTeam = hardcodedWorkforce(tenant);
      const hcAgents = hardcodedSubAgents(tenant);
      return NextResponse.json({
        team: {
          id: hcTeam.id,
          name: hcTeam.name,
          emoji: hcTeam.emoji ?? null,
          spec: hcTeam.spec,
          stages: hcTeam.stages.length,
          enabled: hcTeam.enabled,
        },
        agents: hcAgents.map((a) => ({
          id: a.id,
          name: a.name,
          emoji: a.emoji,
          persona: a.persona,
          toolkits: a.toolkits,
          stage: 0,
          memoryCount: 0,
          botBound: false,
        })),
        counts: { shared: 0, workforce: 0 },
        workforceMemory: [],
        sharedMemory: [],
        files: [],
      });
    }
    const team = await getWorkforce(teamId);
    if (!team || team.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown team" }, { status: 404 });
    }
    const memberIds = Array.from(
      new Set(
        team.stages.flatMap((s) =>
          s.kind === "route" ? s.candidateAgentIds : s.agentIds
        )
      )
    );
    const agents = await getAgentsByIds(memberIds);
    const [counts, wfMem, sharedMem, files, sharedCount, wfCount] =
      await Promise.all([
        Promise.all(
          agents.map((a) =>
            countMemory({ tenantId: tenant, scope: { kind: "agent", agentId: a.id } })
          )
        ),
        listMemory({ tenantId: tenant, scope: { kind: "workforce", workforceId: team.id }, limit: 12 }),
        listMemory({ tenantId: tenant, scope: { kind: "shared" }, limit: 12 }),
        sharedVfsList({ tenantId: tenant, workforceId: team.id }),
        countMemory({ tenantId: tenant, scope: { kind: "shared" } }),
        countMemory({ tenantId: tenant, scope: { kind: "workforce", workforceId: team.id } }),
      ]);

    // Map each agent to the stage index it runs in (for seating order).
    const stageOf = new Map<string, number>();
    team.stages.forEach((s, i) => {
      const ids = s.kind === "route" ? s.candidateAgentIds : s.agentIds;
      ids.forEach((id) => {
        if (!stageOf.has(id)) stageOf.set(id, i);
      });
    });

    return NextResponse.json({
      team: {
        id: team.id,
        name: team.name,
        emoji: team.emoji ?? null,
        spec: team.spec,
        stages: team.stages.length,
        enabled: team.enabled,
      },
      agents: agents.map((a, i) => ({
        id: a.id,
        name: a.name,
        emoji: a.emoji,
        persona: a.persona,
        toolkits: a.toolkits,
        stage: stageOf.get(a.id) ?? 0,
        memoryCount: counts[i],
        botBound: !!a.telegramBotId,
      })),
      counts: { shared: sharedCount, workforce: wfCount },
      workforceMemory: wfMem.map(memOut),
      sharedMemory: sharedMem.map(memOut),
      files,
    });
  }

  // Task-activity dashboard: real recent jobs for a team's member agents,
  // bucketed into Escalated / To review / Errored / All, plus a timeline
  // histogram (24h hourly or past-week daily). All from jobStore — no mocks.
  if (op === "activity") {
    const teamId = url.searchParams.get("teamId") ?? "";
    const range = url.searchParams.get("range") === "week" ? "week" : "24h";

    // Resolve which agentIds count toward this team. The showcase team is not
    // in Redis, so it (and an absent teamId) falls back to tenant-wide jobs.
    let memberIds: string[] | null = null;
    let agentMap = new Map<
      string,
      { name: string; emoji: string }
    >();
    if (teamId && teamId !== HC_TEAM_ID) {
      const team = await getWorkforce(teamId);
      if (!team || team.tenantId !== tenant) {
        return NextResponse.json({ error: "unknown team" }, { status: 404 });
      }
      memberIds = Array.from(
        new Set(
          team.stages.flatMap((s) =>
            s.kind === "route" ? s.candidateAgentIds : s.agentIds
          )
        )
      );
      const members = await getAgentsByIds(memberIds);
      agentMap = new Map(
        members.map((a) => [a.id, { name: a.name, emoji: a.emoji }])
      );
    } else if (teamId === HC_TEAM_ID) {
      for (const a of hardcodedSubAgents(tenant)) {
        agentMap.set(a.id, { name: a.name, emoji: a.emoji });
      }
    }

    const ids = await listRecentJobs(tenant, 100);
    const metas = (await Promise.all(ids.map((id) => getJobMeta(id)))).filter(
      (m): m is JobMeta => !!m && m.tenantId === tenant
    );

    // For a real team, only jobs run by one of its member agents. For the
    // showcase / no team, show all tenant jobs.
    const memberSet = memberIds ? new Set(memberIds) : null;
    const jobs = memberSet
      ? metas.filter((m) => m.agentId && memberSet.has(m.agentId))
      : metas;

    const tasks = jobs.map((m) => {
      const a = m.agentId ? agentMap.get(m.agentId) : undefined;
      return {
        jobId: m.jobId,
        time: m.createdAt,
        prompt:
          m.prompt.length > 160 ? m.prompt.slice(0, 157) + "…" : m.prompt,
        agentId: m.agentId ?? null,
        agentName: a?.name ?? (m.kind === "auto" ? "Automation" : "Agent"),
        agentEmoji: a?.emoji ?? "🤖",
        cost: m.estimatedCost ?? null,
        durationMs:
          m.finishedAt && m.startedAt ? m.finishedAt - m.startedAt : null,
        status: activityStatus(m),
        rawStatus: m.status,
        escalated: !!m.escalated,
      };
    });

    const counts = {
      escalated: tasks.filter((t) => t.status === "Escalated").length,
      toReview: tasks.filter((t) => t.status === "To review").length,
      errored: tasks.filter((t) => t.status === "Error").length,
      all: tasks.length,
    };

    // Timeline histogram.
    const now = Date.now();
    const timeline: Array<{ label: string; count: number }> = [];
    if (range === "week") {
      const day = 86400000;
      for (let i = 6; i >= 0; i--) {
        const start = now - i * day;
        const d = new Date(start);
        const label = d.toLocaleDateString("en-US", { weekday: "short" });
        const lo = now - (i + 1) * day;
        const hi = now - i * day;
        timeline.push({
          label,
          count: tasks.filter((t) => t.time > lo && t.time <= hi).length,
        });
      }
    } else {
      const hour = 3600000;
      for (let i = 23; i >= 0; i--) {
        const lo = now - (i + 1) * hour;
        const hi = now - i * hour;
        const d = new Date(hi);
        const label = `${d.getHours()}`;
        timeline.push({
          label,
          count: tasks.filter((t) => t.time > lo && t.time <= hi).length,
        });
      }
    }

    return NextResponse.json({ tasks, counts, timeline, range });
  }

  // List memories for one scope (shared | agent | workforce).
  if (op === "memory") {
    const scope = await resolveScope(
      tenant,
      url.searchParams.get("scope") ?? "shared",
      url.searchParams.get("agentId") ?? undefined,
      url.searchParams.get("workforceId") ?? undefined
    );
    if (!scope) return NextResponse.json({ error: "bad scope" }, { status: 400 });
    const recs = await listMemory({ tenantId: tenant, scope, limit: 100 });
    return NextResponse.json({ memories: recs.map(memOut) });
  }

  if (op === "triggers") {
    const toolkit = url.searchParams.get("toolkit")?.trim() || undefined;
    const keyword = url.searchParams.get("keyword")?.trim() || undefined;
    const [native, custom, connected] = await Promise.all([
      listTriggerTypes({
        toolkits: toolkit ? [toolkit] : undefined,
        keyword,
        limit: 30,
      }),
      listCustomTriggerTypes(toolkit),
      listConnectedToolkits(tenant),
    ]);
    const kw = (keyword ?? "").toLowerCase();
    const customFiltered = kw
      ? custom.filter(
          (t) =>
            t.slug.toLowerCase().includes(kw) ||
            t.name.toLowerCase().includes(kw) ||
            t.description.toLowerCase().includes(kw)
        )
      : custom;
    return NextResponse.json({
      connected: [...new Set(connected.map((c) => c.toolkitSlug.toLowerCase()))],
      // Custom polling types first so they surface even when Composio returns
      // a wall of natives (monday has ZERO natives — customs are all it has).
      triggers: [
        ...customFiltered.map((t) => ({
          slug: t.slug,
          name: t.name,
          description: t.description.slice(0, 300),
          toolkit: t.toolkit,
          kind: "custom_polling" as const,
          configSchema: t.configSchema,
        })),
        ...native.map((t) => ({
          slug: t.slug,
          name: t.name,
          description: t.description.slice(0, 300),
          toolkit: t.toolkitSlug ?? null,
          kind: "composio" as const,
          configSchema: t.configSchema ?? null,
        })),
      ],
    });
  }

  return NextResponse.json({ error: `unknown op ${op}` }, { status: 400 });
}

function validTrigger(t: unknown): CompiledTrigger | null {
  if (!t || typeof t !== "object") return null;
  const o = t as Record<string, unknown>;
  switch (o.kind) {
    case "schedule": {
      const cron = typeof o.cron === "string" && o.cron.trim() ? o.cron.trim() : undefined;
      const everyMs = typeof o.everyMs === "number" && o.everyMs > 0 ? o.everyMs : undefined;
      if (!cron && !everyMs) return null;
      const tz = typeof o.tz === "string" && o.tz.trim() ? o.tz.trim() : undefined;
      return { kind: "schedule", ...(cron ? { cron } : {}), ...(everyMs ? { everyMs } : {}), ...(tz ? { tz } : {}) };
    }
    case "composio": {
      if (typeof o.triggerType !== "string" || !o.triggerType.trim()) return null;
      return { kind: "composio", triggerType: o.triggerType.trim() };
    }
    case "webhook":
      return { kind: "webhook" };
    case "chat": {
      if (typeof o.pattern !== "string" || !o.pattern.trim()) return null;
      return { kind: "chat", pattern: o.pattern.trim(), flags: "i" };
    }
    default:
      return null;
  }
}

export async function POST(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) {
    return NextResponse.json({ error: "unknown tenant" }, { status: 400 });
  }

  const body = (await req.json().catch(() => null)) as {
    op?: string;
    description?: string;
    trigger?: unknown;
    triggerConfig?: Record<string, unknown>;
    agentId?: string;
    prompt?: string;
    scope?: string;
    workforceId?: string;
    text?: string;
    query?: string;
    source?: string;
    kind?: "note" | "takeaway" | "fact";
    topK?: number;
    modelName?: string | null;
  } | null;

  // --- per-agent model selection ------------------------------------------
  if (body?.op === "set_agent_model") {
    const agentId = (body.agentId ?? "").trim();
    const agent = agentId ? await getSubAgent(agentId) : null;
    if (!agent || agent.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown agent" }, { status: 404 });
    }
    // null / "" clears the override (back to the default routed model). Any
    // non-empty value must be a known catalog id.
    const raw = body.modelName;
    const next = raw == null || raw === "" ? undefined : String(raw);
    if (next !== undefined && !isKnownModel(next)) {
      return NextResponse.json({ error: "unknown model" }, { status: 400 });
    }
    const saved = await putSubAgent({ ...agent, modelName: next });
    return NextResponse.json({ ok: true, modelName: saved.modelName ?? null });
  }

  // --- memory / knowledgebase ops -----------------------------------------
  if (body?.op === "memory_add" || body?.op === "kb_add") {
    const scope = await resolveScope(tenant, body.scope, body.agentId, body.workforceId);
    if (!scope) return NextResponse.json({ error: "bad scope" }, { status: 400 });
    const text = (body.text ?? "").trim();
    if (text.length < 2) {
      return NextResponse.json({ error: "text is required" }, { status: 400 });
    }
    try {
      if (body.op === "kb_add") {
        const out = await addKnowledge({
          tenantId: tenant,
          scope,
          source: (body.source ?? "note").trim() || "note",
          text,
        });
        return NextResponse.json({ ok: true, ...out });
      }
      const rec = await addMemory({
        tenantId: tenant,
        scope,
        text,
        kind: body.kind ?? "note",
        ...(body.source ? { source: body.source } : {}),
      });
      return NextResponse.json({ ok: true, id: rec.id });
    } catch (err: any) {
      return NextResponse.json(
        { error: String(err?.message ?? err).slice(0, 400) },
        { status: 500 }
      );
    }
  }

  if (body?.op === "memory_search") {
    const query = (body.query ?? "").trim();
    if (!query) return NextResponse.json({ error: "query is required" }, { status: 400 });
    // Search across shared + the named agent/workforce scope when provided.
    const scopes: MemoryScope[] = [{ kind: "shared" }];
    if (body.workforceId) {
      const t = await getWorkforce(body.workforceId);
      if (t && t.tenantId === tenant)
        scopes.push({ kind: "workforce", workforceId: body.workforceId });
    }
    if (body.agentId) {
      const a = await getSubAgent(body.agentId);
      if (a && a.tenantId === tenant)
        scopes.push({ kind: "agent", agentId: body.agentId });
    }
    try {
      const hits = await searchMemory({
        tenantId: tenant,
        scopes,
        query,
        topK: Math.min(Math.max(body.topK ?? 8, 1), 25),
      });
      return NextResponse.json({
        hits: hits.map((h) => ({ ...memOut(h.record), score: Number(h.score.toFixed(4)) })),
      });
    } catch (err: any) {
      return NextResponse.json(
        { error: String(err?.message ?? err).slice(0, 400) },
        { status: 500 }
      );
    }
  }

  // Inline canvas chat: run a synchronous, scoped turn AS the agent and return
  // its formatted text. No telegram delivery (showTyping:false skips the
  // streaming/delivery path), but history is kept per-agent so follow-ups in
  // the hover bar have continuity. Isolated session id keeps it off the agent's
  // real bot chats.
  if (body?.op === "ask") {
    const agentId = (body.agentId ?? "").trim();
    const prompt = (body.prompt ?? "").trim();
    if (!agentId || !prompt) {
      return NextResponse.json(
        { error: "agentId and prompt are required" },
        { status: 400 }
      );
    }
    const agent = await getSubAgent(agentId);
    if (!agent || agent.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown agent" }, { status: 404 });
    }
    const sessionId = `webask:${tenant}:${agentId}`;
    try {
      const raw = (await loadHistoryStep(sessionId)) as ModelMessage[];
      const history: ModelMessage[] = Array.isArray(raw) ? raw.slice(-20) : [];
      history.push({ role: "user", content: prompt });

      const result = await agentTurn({
        sessionId,
        userId: tenant,
        channel: "telegram",
        history,
        showTyping: false,
        modelName: resolveModelName("chat"),
        agent: scopeForAgent(agent),
      });

      const text = String((result as { text?: string }).text ?? "").trim();
      history.push({ role: "assistant", content: text });
      await saveHistoryStep(sessionId, history);

      return NextResponse.json({ ok: true, text });
    } catch (err: any) {
      return NextResponse.json(
        { error: String(err?.message ?? err).slice(0, 400) },
        { status: 500 }
      );
    }
  }

  if (body?.op !== "create_workforce") {
    return NextResponse.json({ error: "unknown op" }, { status: 400 });
  }
  const description = (body.description ?? "").trim();
  if (description.length < 10) {
    return NextResponse.json(
      { error: "description must be at least 10 characters" },
      { status: 400 }
    );
  }
  const trigger = body.trigger ? validTrigger(body.trigger) : null;
  if (body.trigger && !trigger) {
    return NextResponse.json({ error: "invalid trigger" }, { status: 400 });
  }

  // The team delivers run summaries to a chat session — resolve the tenant's
  // most recent one (tenant ids are "<channel>:<senderId>").
  const colon = tenant.indexOf(":");
  const channel = (colon > 0 ? tenant.slice(0, colon) : "telegram") as Channel;
  const meta = await getSessionMeta(tenant);
  const last = meta ? null : await getLastSession(channel);
  const sessionId = meta?.sessionId ?? last?.sessionId ?? tenant;

  const baseUrl =
    env("APP_BASE_URL") ??
    (process.env.VERCEL_PROJECT_PRODUCTION_URL
      ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`
      : "https://agentos-claw.vercel.app");

  try {
    const { team, rule, note, newAgents, members } = await createWorkforceFromSpec({
      tenantId: tenant,
      channel,
      sessionId,
      spec: description,
      baseUrl,
      ...(trigger ? { triggerOverride: trigger } : {}),
      ...(body.triggerConfig ? { triggerConfig: body.triggerConfig } : {}),
    });
    return NextResponse.json({
      ok: true,
      teamId: team.id,
      name: team.name,
      emoji: team.emoji ?? null,
      triggerLabel: triggerLabel(rule.trigger),
      note,
      stages: team.stages.length,
      members: members.map((m) => ({ id: m.id, name: m.name, emoji: m.emoji })),
      newAgents: newAgents.map((a) => ({ id: a.id, name: a.name })),
    });
  } catch (err: any) {
    return NextResponse.json(
      { error: String(err?.message ?? err).slice(0, 400) },
      { status: 500 }
    );
  }
}
