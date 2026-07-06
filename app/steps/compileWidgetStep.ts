// app/steps/compileWidgetStep.ts
//
// Natural-language → structured WidgetSpec compiler (the LLM half of the
// hybrid KPI engine). A user types a metric in the dashboard command bar; this
// step asks the meta model to emit a FLAT spec naming the data source, chart,
// metric, window, and — for an external metric — a concrete Composio tool slug
// + args. We then VERIFY a Composio spec by actually running the proposed call
// once, so the deterministic refresh path (widgetExecutor) never needs an LLM.
//
// Flat schema (no z.record): OpenAI strict structured output rejects the
// propertyNames a free-form map generates, so filters/args are JSON strings.
//
// Model: `meta` (gpt-5.4) by default, `meta-pro` (gpt-5.3-codex) on retry —
// never gpt-5.2 (see modelRouting + [[feedback-deep-job-no-gpt52]]).

import { generateObject } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import { executeComposioAction } from "@/app/lib/composioExec";
import {
  type WidgetSpec,
  type WidgetSource,
  type WidgetChart,
  type WidgetMetric,
  type WidgetWindow,
  parseComposioArgs,
  extractByPath,
} from "@/app/lib/widgetSpec";
import { newWidgetId } from "@/app/lib/dashboards";

const compileSchema = z.object({
  title: z.string(),
  source: z.enum(["composio", "jobs", "automations", "evals", "memory"]),
  chart: z.enum(["stat", "line", "bar", "donut", "table"]),
  metric: z.enum(["count", "rate", "cost", "duration", "sum", "avg", "list"]),
  // e.g. "by_status", "by_kind", "by_toolkit"; null for a flat scalar/series.
  dimension: z.string().nullable(),
  window: z.enum(["day", "week", "month", "all"]),
  // JSON-encoded array of {field,op,value}; null for none.
  filtersJson: z.string().nullable(),
  // Composio fields — null unless source === "composio".
  composioToolkit: z.string().nullable(),
  composioToolSlug: z.string().nullable(),
  // JSON-encoded args object for the tool call; null for none.
  composioArgsJson: z.string().nullable(),
  // Dot path into the tool result for the number/series, e.g.
  // "messages" or "data.total".
  extractPath: z.string().nullable(),
});

type CompileFields = z.infer<typeof compileSchema>;

function buildSystem(objective: string | null, connectedToolkits: string[]): string {
  return [
    "You compile a user's natural-language KPI/metric request into a strict,",
    "FLAT structured WidgetSpec. A deterministic engine then refreshes the",
    "widget from the spec WITHOUT calling you again — so be precise.",
    "",
    objective
      ? `Account objective (frames vague requests): ${objective}.`
      : "No account objective set; infer the domain from the request.",
    "",
    "Pick the NARROWEST data source that can answer the request:",
    "  jobs        — this account's deep-job runs (status, cost, duration).",
    "                dimensions: by_status, by_kind. metrics: count, rate",
    "                (done/total), cost (sum USD), duration (avg seconds).",
    "  automations — this account's automation rules + run history.",
    "                dimensions: by_status. metrics: count, rate (ok/total).",
    "  evals       — model eval runs (pass/fail, score).",
    "                dimensions: by_status. metrics: count, rate (pass/total),",
    "                avg (avg grade score).",
    "  memory      — stored memory entries grouped by kind. metric: count.",
    "  composio    — ANY external integration metric (email, ads, CRM, search",
    "                console, support, etc.). Use ONLY when no internal source",
    "                fits.",
    "",
    "Pick a fitting chart: stat (single number), line (a value over time),",
    "bar/donut (a breakdown by a dimension), table (a list of rows). If you set",
    "a dimension, prefer bar/donut; for 'over time' use line; for a single",
    "headline number use stat.",
    "",
    "For a `composio` source you MUST provide a concrete, real Composio action",
    "slug in `composioToolSlug` using the UPPERCASE {TOOLKIT}_{ACTION} naming",
    "convention (e.g. GMAIL_FETCH_EMAILS, GOOGLEADS_..., HUBSPOT_...). Put the",
    "call arguments in `composioArgsJson` as a JSON OBJECT STRING (e.g.",
    '\'{"max_results":1,"query":"is:unread"}\'). Set `composioToolkit` to the',
    "lowercase toolkit slug (e.g. 'gmail'). Set `extractPath` to a dot path",
    "into the result that holds the number or the array to count/aggregate.",
    connectedToolkits.length
      ? `Connected toolkits for this account: ${connectedToolkits.join(", ")}. Prefer these.`
      : "No external toolkits are connected; avoid the composio source if possible.",
    "",
    "For internal sources, set all composio* fields and extractPath to null.",
    "Set `filtersJson` to null unless the user clearly asked to filter. Always",
    "set a short human `title`. Set every field you are not using to null.",
  ].join("\n");
}

function clampSource(s: string): WidgetSource {
  const ok = ["composio", "jobs", "automations", "evals", "memory"];
  return (ok.includes(s) ? s : "jobs") as WidgetSource;
}

export async function compileWidgetStep(args: {
  tenantId: string;
  prompt: string;
  objective?: string | null;
  connectedToolkits?: string[];
  retry?: boolean;
}): Promise<WidgetSpec> {
  "use step";

  const llm = buildLlmArgs({
    purpose: args.retry ? "meta-pro" : "meta",
    temperature: 0.2,
  });

  const result = await generateObject({
    model: (llm as any).model,
    ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
    ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
    schema: compileSchema,
    system: buildSystem(args.objective ?? null, args.connectedToolkits ?? []),
    prompt: `Compile this metric request into a WidgetSpec:\n\n${args.prompt}`,
  });

  const o: CompileFields = result.object;
  const now = Date.now();
  const source = clampSource(o.source);

  const spec: WidgetSpec = {
    id: newWidgetId(),
    tenantId: args.tenantId,
    title: o.title?.trim() || args.prompt.slice(0, 60),
    prompt: args.prompt,
    source,
    chart: (o.chart ?? "stat") as WidgetChart,
    metric: (o.metric ?? "count") as WidgetMetric,
    dimension: o.dimension?.trim() || null,
    window: (o.window ?? "week") as WidgetWindow,
    filtersJson: o.filtersJson?.trim() || null,
    composioToolkit: source === "composio" ? o.composioToolkit?.trim() || null : null,
    composioToolSlug: source === "composio" ? o.composioToolSlug?.trim() || null : null,
    composioArgsJson: source === "composio" ? o.composioArgsJson?.trim() || null : null,
    extractPath: source === "composio" ? o.extractPath?.trim() || null : null,
    createdAt: now,
    updatedAt: now,
  };

  // Verify a Composio spec by running the proposed call ONCE. Success means the
  // slug + args are real and refresh stays deterministic; failure is recorded
  // as lastError (the widget still saves, surfacing the error in its card).
  if (spec.source === "composio") {
    if (!spec.composioToolSlug) {
      spec.lastError = "compiler did not produce a Composio tool slug";
    } else {
      const verify = await executeComposioAction(
        spec.tenantId,
        spec.composioToolSlug,
        parseComposioArgs(spec.composioArgsJson)
      );
      if (!verify.ok) {
        spec.lastError = `tool verification failed: ${(verify.error ?? "unknown").slice(0, 180)}`;
      } else {
        // Confirm the extractPath actually resolves to something usable; clear
        // it if not, so the executor falls back to the whole payload.
        const extracted = extractByPath(verify.data, spec.extractPath);
        if (extracted == null && spec.extractPath) spec.extractPath = null;
      }
    }
  }

  return spec;
}
