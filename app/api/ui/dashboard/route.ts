// app/api/ui/dashboard/route.ts
//
// Data + mutation API for the AI-composable dashboard. Tenant-scoped, same
// auth as the rest of /api/ui. The hybrid KPI engine lives behind this:
// `create`/`recompile` call the LLM compiler once; everything else (list,
// data, refresh) is the deterministic executor + cache.
//
//   GET  ?op=list                 → widgets (specs) + their cached data
//   GET  ?op=data&id=<id>         → force a fresh runWidgetSpec for one widget
//   GET  ?op=objective            → objective + suggested prompts + kinds
//   POST {op:"create", prompt}    → compile → save → run once → {widget,data}
//   POST {op:"recompile", id}     → re-compile the same prompt in place
//   POST {op:"update", id, title} → rename
//   POST {op:"delete", id}        → remove
//   POST {op:"reorder", ids}      → persist render order
//   POST {op:"set_objective", kind, label?, headline?}

import { NextResponse } from "next/server";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import { listConnectedToolkits } from "@/app/lib/composioConnections";
import {
  getObjective,
  setObjective,
  suggestedWidgetPrompts,
  OBJECTIVE_KINDS,
  type ObjectiveKind,
} from "@/app/lib/accountObjective";
import {
  listWidgets,
  getWidget,
  putWidget,
  deleteWidget,
  reorderWidgets,
  getCachedData,
  newWidgetId,
} from "@/app/lib/dashboards";
import type { WidgetSpec } from "@/app/lib/widgetSpec";
import { runWidgetSpec } from "@/app/lib/widgetExecutor";
import { compileWidgetStep } from "@/app/steps/compileWidgetStep";
import {
  listRecent as listRecentMemories,
  countByKind,
  deleteMemory,
} from "@/app/lib/memoryStore";
import { listSolutions, deleteSolution } from "@/app/lib/solutionMemory";
import { isTenantPaused, setTenantPaused } from "@/app/lib/tenantPause";

export const dynamic = "force-dynamic";
export const maxDuration = 300;

const VALID_KINDS = new Set<string>(OBJECTIVE_KINDS.map((k) => k.kind));

function objectivePayload(tenant: string, obj: Awaited<ReturnType<typeof getObjective>>) {
  const kind: ObjectiveKind = obj?.kind ?? "custom";
  return {
    objective: obj,
    suggestions: suggestedWidgetPrompts(kind),
    kinds: OBJECTIVE_KINDS,
  };
}

export async function GET(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) return NextResponse.json({ error: "unknown tenant" }, { status: 400 });

  const op = url.searchParams.get("op") ?? "list";

  if (op === "objective") {
    const obj = await getObjective(tenant);
    return NextResponse.json({ ok: true, ...objectivePayload(tenant, obj) });
  }

  if (op === "data") {
    const id = url.searchParams.get("id") ?? "";
    const spec = await getWidget(id);
    if (!spec || spec.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown widget" }, { status: 404 });
    }
    const data = await runWidgetSpec(tenant, spec, { forceFresh: true });
    return NextResponse.json({ ok: true, id, data });
  }

  if (op === "pause") {
    return NextResponse.json({ ok: true, paused: await isTenantPaused(tenant) });
  }

  if (op === "memory") {
    const [counts, recent, solutions] = await Promise.all([
      countByKind(tenant),
      listRecentMemories(tenant, 30),
      listSolutions({ tenantId: tenant, limit: 40 }),
    ]);
    return NextResponse.json({ ok: true, counts, recent, solutions });
  }

  // op === "list"
  const widgets = await listWidgets(tenant);
  const data = await Promise.all(widgets.map((w) => getCachedData(w.id)));
  return NextResponse.json({
    ok: true,
    widgets,
    data: Object.fromEntries(widgets.map((w, i) => [w.id, data[i]])),
  });
}

export async function POST(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) return NextResponse.json({ error: "unknown tenant" }, { status: 400 });

  const body = (await req.json().catch(() => null)) as {
    op?: string;
    prompt?: string;
    id?: string;
    title?: string;
    ids?: string[];
    kind?: string;
    label?: string;
    headline?: string;
    scope?: string;
    paused?: boolean;
  } | null;

  const op = body?.op;

  if (op === "set_pause") {
    const paused = await setTenantPaused(tenant, body?.paused === true);
    return NextResponse.json({ ok: true, paused });
  }

  if (op === "mem_delete") {
    const id = (body?.id ?? "").trim();
    if (!id) return NextResponse.json({ error: "id required" }, { status: 400 });
    if (body?.scope === "solution") {
      await deleteSolution(id);
    } else {
      await deleteMemory(tenant, id);
    }
    return NextResponse.json({ ok: true, id });
  }

  if (op === "set_objective") {
    const kind = body?.kind ?? "";
    if (!VALID_KINDS.has(kind)) {
      return NextResponse.json({ error: "invalid objective kind" }, { status: 400 });
    }
    const obj = await setObjective(tenant, {
      kind: kind as ObjectiveKind,
      label: body?.label,
      headline: body?.headline,
    });
    return NextResponse.json({ ok: true, ...objectivePayload(tenant, obj) });
  }

  if (op === "delete") {
    const id = (body?.id ?? "").trim();
    const spec = id ? await getWidget(id) : null;
    if (!spec || spec.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown widget" }, { status: 404 });
    }
    await deleteWidget(tenant, id);
    return NextResponse.json({ ok: true, id });
  }

  if (op === "reorder") {
    const ids = Array.isArray(body?.ids) ? body!.ids : [];
    await reorderWidgets(tenant, ids);
    return NextResponse.json({ ok: true });
  }

  // Hand-seed internal-source widgets with NO LLM call. Smoke-tests the
  // deterministic executor + chart pipeline against real tenant data (jobs,
  // automations, evals, memory) independent of the compiler.
  if (op === "seed_demo") {
    const now = Date.now();
    const base = {
      tenantId: tenant,
      filtersJson: null,
      composioToolkit: null,
      composioToolSlug: null,
      composioArgsJson: null,
      extractPath: null,
      createdAt: now,
      updatedAt: now,
    } as const;
    const seeds: WidgetSpec[] = [
      { ...base, id: newWidgetId(), title: "Jobs by status", prompt: "jobs by status", source: "jobs", chart: "donut", metric: "count", dimension: "by_status", window: "week" },
      { ...base, id: newWidgetId(), title: "Automation success rate", prompt: "automation success rate this week", source: "automations", chart: "stat", metric: "rate", dimension: null, window: "week" },
      { ...base, id: newWidgetId(), title: "Eval pass rate", prompt: "eval pass rate this month", source: "evals", chart: "stat", metric: "rate", dimension: null, window: "month" },
      { ...base, id: newWidgetId(), title: "Memory by kind", prompt: "memory entries by kind", source: "memory", chart: "bar", metric: "count", dimension: "by_kind", window: "all" },
    ];
    const out = await Promise.all(
      seeds.map(async (spec) => {
        await putWidget(spec);
        const data = await runWidgetSpec(tenant, spec, { forceFresh: true });
        return { widget: spec, data };
      })
    );
    return NextResponse.json({ ok: true, seeded: out });
  }

  if (op === "update") {
    const id = (body?.id ?? "").trim();
    const spec = id ? await getWidget(id) : null;
    if (!spec || spec.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown widget" }, { status: 404 });
    }
    const title = (body?.title ?? "").trim();
    if (title) spec.title = title.slice(0, 80);
    spec.updatedAt = Date.now();
    await putWidget(spec);
    return NextResponse.json({ ok: true, widget: spec });
  }

  if (op === "create" || op === "recompile") {
    const isRecompile = op === "recompile";
    let prompt: string;
    let preserveId: string | undefined;
    let createdAt: number | undefined;

    if (isRecompile) {
      const id = (body?.id ?? "").trim();
      const existing = id ? await getWidget(id) : null;
      if (!existing || existing.tenantId !== tenant) {
        return NextResponse.json({ error: "unknown widget" }, { status: 404 });
      }
      prompt = existing.prompt;
      preserveId = existing.id;
      createdAt = existing.createdAt;
    } else {
      prompt = (body?.prompt ?? "").trim();
      if (prompt.length < 3) {
        return NextResponse.json({ error: "prompt is required" }, { status: 400 });
      }
    }

    try {
      const [obj, connected] = await Promise.all([
        getObjective(tenant),
        listConnectedToolkits(tenant),
      ]);
      const objectiveText = obj ? `${obj.label}${obj.headline ? ` — ${obj.headline}` : ""}` : null;
      const toolkits = [...new Set(connected.map((c) => c.toolkitSlug.toLowerCase()))];

      let spec = await compileWidgetStep({
        tenantId: tenant,
        prompt,
        objective: objectiveText,
        connectedToolkits: toolkits,
      });

      // Recompile keeps the widget's identity + position.
      if (preserveId) {
        spec = { ...spec, id: preserveId, createdAt: createdAt ?? spec.createdAt };
      }

      await putWidget(spec);
      const data = await runWidgetSpec(tenant, spec, { forceFresh: true });
      return NextResponse.json({ ok: true, widget: spec, data });
    } catch (err: any) {
      return NextResponse.json(
        { error: String(err?.message ?? err).slice(0, 400) },
        { status: 500 }
      );
    }
  }

  return NextResponse.json({ error: "unknown op" }, { status: 400 });
}
