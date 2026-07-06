// app/lib/widgetSpec.ts
//
// The saved, structured description of a dashboard widget. An LLM compiles a
// natural-language prompt into one of these ONCE (see compileWidgetStep); the
// deterministic executor (widgetExecutor) then refreshes it without any LLM
// call. That's the "hybrid" engine: LLM cost on create/edit only.
//
// The spec is intentionally FLAT and JSON-serializable so it round-trips
// through OpenAI strict structured output (no z.record / nested maps — those
// generate `propertyNames`/`additionalProperties` that strict mode rejects).
// Maps and arrays are carried as JSON-encoded string fields.

export type WidgetSource =
  | "composio"
  | "jobs"
  | "automations"
  | "evals"
  | "memory";

export type WidgetChart = "stat" | "line" | "bar" | "donut" | "table";

export type WidgetMetric =
  | "count"
  | "rate"
  | "cost"
  | "duration"
  | "sum"
  | "avg"
  | "list";

export type WidgetWindow = "day" | "week" | "month" | "all";

export type WidgetSpec = {
  id: string;
  tenantId: string;
  title: string;
  // The original natural-language prompt, kept for display + recompile.
  prompt: string;

  source: WidgetSource;
  chart: WidgetChart;
  metric: WidgetMetric;
  // What to break the metric down by, e.g. "by_status", "by_kind",
  // "by_toolkit". null = no breakdown (a single scalar / flat list).
  dimension: string | null;
  window: WidgetWindow;
  // JSON-encoded array of {field,op,value} filters. null = no filters.
  filtersJson: string | null;

  // Composio-source fields (null for internal sources). Verified once at
  // compile time so refresh is a deterministic single tool call.
  composioToolkit: string | null;
  composioToolSlug: string | null;
  // JSON-encoded args object for the tool call.
  composioArgsJson: string | null;
  // Dot path into the tool result to pull the number/series out of.
  extractPath: string | null;

  createdAt: number;
  updatedAt: number;
  // Last compile/execute error (e.g. a Composio call that stopped resolving).
  lastError?: string;
};

// The deterministic output the executor produces from a spec. The chart
// component switches on `kind`.
export type WidgetData = {
  kind: WidgetChart;
  // stat
  value?: number | string;
  unit?: string;
  delta?: number;
  // line / bar / donut
  series?: { label: string; value: number }[];
  // table
  columns?: string[];
  rows?: string[][];
  updatedAt: number;
  // Served from cache past its TTL because a fresh compute failed.
  stale?: boolean;
  error?: string;
};

const WINDOW_MS: Record<WidgetWindow, number> = {
  day: 24 * 60 * 60 * 1000,
  week: 7 * 24 * 60 * 60 * 1000,
  month: 30 * 24 * 60 * 60 * 1000,
  all: Number.POSITIVE_INFINITY,
};

// Lower bound (epoch ms) for a spec's window. `all` → 0.
export function windowSinceMs(window: WidgetWindow, now = Date.now()): number {
  const span = WINDOW_MS[window];
  return span === Number.POSITIVE_INFINITY ? 0 : now - span;
}

export function parseFilters(
  filtersJson: string | null
): { field: string; op: string; value: unknown }[] {
  if (!filtersJson) return [];
  try {
    const arr = JSON.parse(filtersJson);
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

export function parseComposioArgs(argsJson: string | null): Record<string, unknown> {
  if (!argsJson) return {};
  try {
    const obj = JSON.parse(argsJson);
    return obj && typeof obj === "object" ? obj : {};
  } catch {
    return {};
  }
}

// Walk a dot/bracket path ("data.items.0.count") into a nested value.
export function extractByPath(root: unknown, path: string | null): unknown {
  if (!path) return root;
  const parts = path
    .replace(/\[(\w+)\]/g, ".$1")
    .split(".")
    .map((p) => p.trim())
    .filter(Boolean);
  let cur: any = root;
  for (const part of parts) {
    if (cur == null) return undefined;
    cur = cur[part];
  }
  return cur;
}
