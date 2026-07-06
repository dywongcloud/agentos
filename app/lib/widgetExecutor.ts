// app/lib/widgetExecutor.ts
//
// Deterministic, cache-aware executor for a compiled WidgetSpec. No LLM —
// given a spec it reads the relevant internal store (jobs / automations /
// evals / memory) or replays the verified Composio tool call, aggregates per
// the spec's metric/dimension/window, and returns a WidgetData the chart
// primitives render. This is the "refresh" half of the hybrid engine.

import {
  type WidgetSpec,
  type WidgetData,
  type WidgetSource,
  windowSinceMs,
  parseComposioArgs,
  extractByPath,
} from "@/app/lib/widgetSpec";
import { getCachedData, setCachedData, DEFAULT_WIDGET_TTL_SECONDS } from "@/app/lib/dashboards";

import { listRecentJobs, getJobMeta } from "@/app/lib/jobStore";
import { listByTenant, listRunsByTenant } from "@/app/lib/automations";
import { listRuns as listEvalRuns } from "@/app/lib/evals/store";
import { countByKind } from "@/app/lib/memoryStore";
import { executeComposioAction } from "@/app/lib/composioExec";

// A normalized event the generic aggregators understand. Each source maps its
// records onto this shape so metric/dimension/window logic lives in one place.
type Ev = {
  ts: number;
  status?: string;
  cost?: number;
  durationMs?: number;
  score?: number;
  group?: string;
  cols?: string[];
};

// Which status counts as "success" for the rate metric, per source.
const SUCCESS_STATUS: Partial<Record<WidgetSource, string>> = {
  jobs: "done",
  automations: "ok",
  evals: "pass",
};

const DAY_MS = 24 * 60 * 60 * 1000;

function startOfDay(ts: number): number {
  return Math.floor(ts / DAY_MS) * DAY_MS;
}

function dayLabel(ts: number): string {
  return new Date(ts).toISOString().slice(5, 10); // MM-DD
}

function histogram(events: Ev[]): { label: string; value: number }[] {
  const map = new Map<string, number>();
  for (const e of events) {
    const k = e.group ?? "unknown";
    map.set(k, (map.get(k) ?? 0) + 1);
  }
  return [...map.entries()]
    .map(([label, value]) => ({ label, value }))
    .sort((a, b) => b.value - a.value);
}

function dailySeries(events: Ev[], sinceMs: number): { label: string; value: number }[] {
  // Bucket into the last ~14 days (or since the window start, whichever is
  // shorter) so a line chart has a stable, readable x-axis.
  const now = Date.now();
  const from = sinceMs > 0 ? startOfDay(sinceMs) : startOfDay(now - 13 * DAY_MS);
  const buckets = new Map<number, number>();
  for (let d = from; d <= now; d += DAY_MS) buckets.set(d, 0);
  for (const e of events) {
    const d = startOfDay(e.ts);
    if (buckets.has(d)) buckets.set(d, (buckets.get(d) ?? 0) + 1);
  }
  return [...buckets.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([ts, value]) => ({ label: dayLabel(ts), value }));
}

function aggregateScalar(
  events: Ev[],
  spec: WidgetSpec
): { value: number | string; unit?: string } {
  const success = SUCCESS_STATUS[spec.source];
  switch (spec.metric) {
    case "rate": {
      if (events.length === 0) return { value: 0, unit: "%" };
      const ok = success ? events.filter((e) => e.status === success).length : 0;
      return { value: Math.round((ok / events.length) * 100), unit: "%" };
    }
    case "cost": {
      const sum = events.reduce((a, e) => a + (e.cost ?? 0), 0);
      return { value: Number(sum.toFixed(2)), unit: "USD" };
    }
    case "duration": {
      const durs = events.map((e) => e.durationMs).filter((d): d is number => d != null);
      if (durs.length === 0) return { value: 0, unit: "s" };
      const avg = durs.reduce((a, d) => a + d, 0) / durs.length;
      return { value: Math.round(avg / 1000), unit: "s" };
    }
    case "sum": {
      const sum = events.reduce((a, e) => a + (e.score ?? 0), 0);
      return { value: Number(sum.toFixed(2)) };
    }
    case "avg": {
      const scores = events.map((e) => e.score).filter((s): s is number => s != null);
      if (scores.length === 0) return { value: 0 };
      return { value: Number((scores.reduce((a, s) => a + s, 0) / scores.length).toFixed(2)) };
    }
    case "count":
    case "list":
    default:
      return { value: events.length };
  }
}

// Shape a normalized event list into the WidgetData the spec's chart wants.
function shapeData(events: Ev[], spec: WidgetSpec): WidgetData {
  const sinceMs = windowSinceMs(spec.window);
  const inWindow = events.filter((e) => e.ts >= sinceMs);
  const updatedAt = Date.now();

  switch (spec.chart) {
    case "line":
      return { kind: "line", series: dailySeries(inWindow, sinceMs), updatedAt };
    case "bar":
    case "donut":
      return { kind: spec.chart, series: histogram(inWindow), updatedAt };
    case "table": {
      const rows = inWindow.slice(0, 20).map((e) => e.cols ?? [String(e.ts)]);
      return { kind: "table", rows, updatedAt };
    }
    case "stat":
    default: {
      const { value, unit } = aggregateScalar(inWindow, spec);
      return { kind: "stat", value, unit, updatedAt };
    }
  }
}

async function jobsEvents(tenantId: string): Promise<Ev[]> {
  const ids = await listRecentJobs(tenantId, 200);
  const metas = await Promise.all(ids.map((id) => getJobMeta(id)));
  const out: Ev[] = [];
  for (const m of metas) {
    if (!m) continue;
    out.push({
      ts: m.createdAt,
      status: m.status,
      cost: m.estimatedCost,
      durationMs: m.finishedAt ? m.finishedAt - m.createdAt : undefined,
      group: m.status,
      cols: [
        m.jobId.slice(0, 10),
        m.kind,
        m.status,
        new Date(m.createdAt).toISOString().slice(0, 16).replace("T", " "),
      ],
    });
  }
  return out;
}

async function automationEvents(tenantId: string): Promise<Ev[]> {
  const [rules, runs] = await Promise.all([
    listByTenant(tenantId),
    listRunsByTenant(tenantId, 200),
  ]);
  const nameById = new Map(rules.map((r) => [r.id, r.name]));
  return runs.map((r) => ({
    ts: r.ts,
    status: r.status,
    group: r.status,
    cols: [
      nameById.get(r.automationId) ?? r.automationId.slice(0, 10),
      r.source,
      r.status,
      new Date(r.ts).toISOString().slice(0, 16).replace("T", " "),
    ],
  }));
}

async function evalEvents(spec: WidgetSpec): Promise<Ev[]> {
  const sinceMs = windowSinceMs(spec.window);
  const runs = await listEvalRuns({ sinceMs: sinceMs > 0 ? sinceMs : undefined, limit: 200 });
  return runs.map((r) => {
    const scores = r.grades.map((g) => g.score).filter((s): s is number => typeof s === "number");
    const avg = scores.length ? scores.reduce((a, s) => a + s, 0) / scores.length : undefined;
    return {
      ts: r.ts,
      status: r.status,
      score: avg,
      group: r.status,
      cols: [r.caseId.slice(0, 18), r.suite, r.status],
    };
  });
}

async function memoryData(tenantId: string, spec: WidgetSpec): Promise<WidgetData> {
  const counts = await countByKind(tenantId);
  const entries = Object.entries(counts).filter(([, v]) => v > 0);
  const total = entries.reduce((a, [, v]) => a + v, 0);
  const updatedAt = Date.now();
  if (spec.chart === "stat") {
    return { kind: "stat", value: total, unit: "entries", updatedAt };
  }
  if (spec.chart === "table") {
    return { kind: "table", columns: ["kind", "count"], rows: entries.map(([k, v]) => [k, String(v)]), updatedAt };
  }
  const series = entries.map(([label, value]) => ({ label, value })).sort((a, b) => b.value - a.value);
  return { kind: spec.chart === "line" ? "bar" : spec.chart, series, updatedAt };
}

// Replay the verified Composio tool call and aggregate its result.
async function composioData(tenantId: string, spec: WidgetSpec): Promise<WidgetData> {
  const updatedAt = Date.now();
  if (!spec.composioToolSlug) {
    return { kind: spec.chart, updatedAt, error: "no composio tool resolved" };
  }
  const args = parseComposioArgs(spec.composioArgsJson);
  const res = await executeComposioAction(tenantId, spec.composioToolSlug, args);
  if (!res.ok) {
    return { kind: spec.chart, updatedAt, error: res.error ?? "composio call failed" };
  }
  const extracted = extractByPath(res.data, spec.extractPath);

  if (Array.isArray(extracted)) {
    if (spec.chart === "stat") {
      return { kind: "stat", value: extracted.length, updatedAt };
    }
    if (spec.chart === "table") {
      const rows = extracted.slice(0, 20).map((x) =>
        typeof x === "object" && x ? Object.values(x).map((v) => String(v)).slice(0, 4) : [String(x)]
      );
      return { kind: "table", rows, updatedAt };
    }
    const series = extracted.slice(0, 12).map((x: any, i: number) => ({
      label: String(x?.label ?? x?.name ?? i + 1),
      value: Number(x?.value ?? x?.count ?? 1) || 1,
    }));
    return { kind: spec.chart, series, updatedAt };
  }

  const num = typeof extracted === "number" ? extracted : Number(extracted);
  return {
    kind: spec.chart === "stat" ? "stat" : "stat",
    value: Number.isFinite(num) ? num : String(extracted ?? "—"),
    updatedAt,
  };
}

async function computeFresh(tenantId: string, spec: WidgetSpec): Promise<WidgetData> {
  switch (spec.source) {
    case "jobs":
      return shapeData(await jobsEvents(tenantId), spec);
    case "automations":
      return shapeData(await automationEvents(tenantId), spec);
    case "evals":
      return shapeData(await evalEvents(spec), spec);
    case "memory":
      return memoryData(tenantId, spec);
    case "composio":
      return composioData(tenantId, spec);
    default:
      return { kind: spec.chart, updatedAt: Date.now(), error: `unknown source ${spec.source}` };
  }
}

// Cache-aware entry point. forceFresh bypasses the cache (manual refresh).
export async function runWidgetSpec(
  tenantId: string,
  spec: WidgetSpec,
  opts: { forceFresh?: boolean; ttlSeconds?: number } = {}
): Promise<WidgetData> {
  if (!opts.forceFresh) {
    const cached = await getCachedData(spec.id);
    if (cached) return cached;
  }

  try {
    const data = await computeFresh(tenantId, spec);
    await setCachedData(spec.id, data, opts.ttlSeconds ?? DEFAULT_WIDGET_TTL_SECONDS);
    return data;
  } catch (err: any) {
    // Compute failed — serve the last good cache (marked stale) if we have one.
    const stale = await getCachedData(spec.id);
    if (stale) return { ...stale, stale: true, error: String(err?.message ?? err).slice(0, 200) };
    return { kind: spec.chart, updatedAt: Date.now(), error: String(err?.message ?? err).slice(0, 200) };
  }
}
