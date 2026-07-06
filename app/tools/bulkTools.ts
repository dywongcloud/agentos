// app/tools/bulkTools.ts
//
// Agent tools for the durable bulk-run engine (app/lib/bulkRuns.ts). The agent
// sets a run up ONCE — which read fetches the rows, which action runs per row,
// and how columns map into args — then the engine executes deterministically
// with checkpointing, per-item retries, idempotency, and rate limiting. The
// agent must NOT try to loop large datasets itself with per-item tool calls.

import { tool } from "ai";
import { z } from "zod/v4";

import type { Channel } from "@/app/lib/identity";
import {
  createBulkRun,
  getBulkRun,
  listBulkFailures,
  resetBulkFailuresForRetry,
} from "@/app/lib/bulkRuns";

export type BulkToolContext = {
  tenantId: string;
  channel: Channel;
  sessionId: string;
};

function parseJsonObject(raw: string | null | undefined): Record<string, unknown> | null {
  if (!raw || !raw.trim()) return null;
  try {
    const v = JSON.parse(raw);
    return v && typeof v === "object" && !Array.isArray(v)
      ? (v as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

export function makeStartBulkRunTool(ctx: BulkToolContext) {
  return tool({
    description: [
      "Start a DURABLE bulk run over a large dataset (hundreds/thousands of",
      "rows): one Composio READ fetches the rows, then one Composio ACTION runs",
      "per row with automatic batching, per-item retries, idempotency (a row",
      "that succeeded is never repeated), rate limiting, progress reports and a",
      "final summary. USE THIS instead of looping tool calls yourself whenever",
      "an operation must repeat over more than ~10 items — e.g. 'email every",
      "contact in tab 2 of this sheet using the pitch in their row'.",
      "",
      "fetch_tool/fetch_args: the read call (e.g. GOOGLESHEETS_BATCH_GET with",
      "the spreadsheet id and a range covering the whole tab, like",
      "'Tab Name'!A1:Z2000). Get the exact arg names from",
      "COMPOSIO_GET_TOOL_SCHEMAS first. Rows with a header line become",
      "{column: value} objects automatically.",
      "",
      "action_args_template: JSON object whose string values may reference row",
      "columns as {{Column Header}} (case-insensitive), e.g.",
      '{"recipient_email":"{{Email}}","subject":"{{Subject}}","body":"{{Pitch}}"}.',
      "Rows missing a referenced column are skipped, not failed.",
      "",
      "SAFETY: for irreversible actions (emails, messages), run dry_run:true",
      "first and show the user the counts, and confirm with the user before the",
      "real run unless they already clearly instructed it.",
      "The run reports progress in chat; /stop halts it between batches.",
    ].join("\n"),
    inputSchema: z.object({
      description: z.string().min(1).describe("Short human label, e.g. 'pitch emails to tab-2 contacts'."),
      fetch_tool: z.string().min(1).describe("Composio READ slug, e.g. GOOGLESHEETS_BATCH_GET."),
      fetch_args: z.string().min(2).describe("JSON object string of the read call's arguments."),
      items_path: z
        .string()
        .nullable()
        .describe("Optional dot-path to the rows array in the response; null to auto-detect."),
      header_row: z
        .boolean()
        .nullable()
        .describe("Whether row 1 is a header row (default true for sheet-style data)."),
      action_tool: z.string().min(1).describe("Composio ACTION slug to run per row, e.g. GMAIL_SEND_EMAIL."),
      action_args_template: z
        .string()
        .min(2)
        .describe("JSON object string; values may use {{Column Header}} placeholders."),
      max_items: z.number().nullable().describe("Safety cap on rows processed (default 2000)."),
      dry_run: z
        .boolean()
        .nullable()
        .describe("true = resolve every row and count sends WITHOUT executing anything."),
    }),
    execute: async (args) => {
      const fetchArgs = parseJsonObject(args.fetch_args);
      if (!fetchArgs) return { ok: false, error: "fetch_args is not a valid JSON object string" };
      const tplRaw = parseJsonObject(args.action_args_template);
      if (!tplRaw) return { ok: false, error: "action_args_template is not a valid JSON object string" };
      const argsTemplate: Record<string, string> = {};
      for (const [k, v] of Object.entries(tplRaw)) {
        argsTemplate[k] = typeof v === "string" ? v : JSON.stringify(v);
      }

      const run = await createBulkRun({
        tenantId: ctx.tenantId,
        channel: ctx.channel,
        sessionId: ctx.sessionId,
        description: args.description,
        fetch: {
          tool: args.fetch_tool.trim(),
          args: fetchArgs,
          ...(args.items_path ? { itemsPath: args.items_path } : {}),
          ...(args.header_row === false ? { headerRow: false } : {}),
        },
        action: { tool: args.action_tool.trim(), argsTemplate },
        ...(args.max_items ? { maxItems: args.max_items } : {}),
        dryRun: args.dry_run ?? false,
      });

      // start() must not run inside a workflow body — tools execute inside a
      // step (agentTurn), same pattern as createAutomationJobStep.
      const { start } = await import("workflow/api");
      const { bulkWorkflow } = await import("@/app/workflows/bulkWorkflow");
      await start(bulkWorkflow, [run.id]);

      return {
        ok: true,
        run_id: run.id,
        dry_run: run.dryRun,
        message:
          `Bulk run ${run.id} launched${run.dryRun ? " in DRY-RUN mode" : ""}. It runs in the ` +
          `background with progress updates in chat — tell the user it's underway ` +
          `(do not wait for it or re-check in this turn).`,
      };
    },
  });
}

export function makeBulkRunStatusTool(ctx: BulkToolContext) {
  return tool({
    description:
      "Check a bulk run's progress/result by id (bulk_...). Also supports retrying " +
      "a finished run's FAILED rows only: pass retry_failed:true — succeeded rows " +
      "are never repeated.",
    inputSchema: z.object({
      run_id: z.string().min(1),
      retry_failed: z.boolean().nullable().describe("true = re-attempt only the failed rows of a finished run."),
    }),
    execute: async (args) => {
      const run = await getBulkRun(args.run_id.trim());
      if (!run || run.tenantId !== ctx.tenantId) {
        return { ok: false, error: `no bulk run ${args.run_id} for this account` };
      }
      if (args.retry_failed) {
        if (run.status === "running" || run.status === "pending") {
          return { ok: false, error: "run is still in progress — wait for it to finish first" };
        }
        const retryable = await resetBulkFailuresForRetry(run.id);
        if (retryable === 0) return { ok: false, error: "no failed rows to retry" };
        const { start } = await import("workflow/api");
        const { bulkWorkflow } = await import("@/app/workflows/bulkWorkflow");
        await start(bulkWorkflow, [run.id]);
        return {
          ok: true,
          run_id: run.id,
          retrying: retryable,
          message: `Retrying ${retryable} failed row(s) — succeeded rows are skipped automatically.`,
        };
      }
      const failures = run.failed > 0 ? await listBulkFailures(run.id, 5) : [];
      return {
        ok: true,
        run_id: run.id,
        status: run.status,
        total: run.total,
        done: run.done,
        failed: run.failed,
        skipped: run.skipped,
        dry_run: run.dryRun,
        ...(run.error ? { error: run.error } : {}),
        ...(failures.length ? { recent_failures: failures } : {}),
      };
    },
  });
}
