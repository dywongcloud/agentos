// app/tools/logsTool.ts
//
// "view_logs" — the agent answers "what did I do recently?" / "show me my
// recent jobs" / "what did you subscribe me to?" from the per-tenant
// activity log. No LLM in the read path; same query returns same result.
//
// For Composio's own audit/connection logs the agent should use the
// existing `composio_api_request` tool to hit /audit_logs or similar
// endpoints — we don't reimplement that here.

import { tool } from "ai";
import { z } from "zod/v4";

import { listActivity, countActivity } from "@/app/lib/activityLog";

export type LogsToolContext = {
  tenantId: string;
};

export function makeViewLogsTool(ctx: LogsToolContext) {
  return tool({
    description: [
      "Read this user's recent activity log: tool calls the agent made on",
      "their behalf, jobs dispatched, memories written, triggers subscribed,",
      "browser sessions captured, code tasks run. Use when:",
      "  - User asks 'what have I done lately?' / 'show me my history'",
      "  - You need to remind yourself what a previous step produced",
      "  - User is debugging 'why didn't that work?' and you want context",
      "",
      "Optional filters:",
      "  kind        — one of tool, job, command, memory, trigger, login,",
      "                code, browse, system. Default: all kinds.",
      "  hours_back  — only entries from the last N hours. Default: all.",
      "  contains    — substring filter on the summary line.",
      "",
      "For Composio's own audit log (connection-events at the platform",
      "level), use composio_api_request to GET /audit_logs/list instead.",
    ].join("\n"),
    inputSchema: z.object({
      kind: z
        .enum([
          "tool",
          "job",
          "command",
          "memory",
          "trigger",
          "login",
          "code",
          "browse",
          "system",
        ])
        .nullable(),
      hours_back: z.number().min(0).max(24 * 30).nullable(),
      contains: z.string().nullable(),
      limit: z.number().int().min(1).max(200).nullable(),
    }),
    execute: async (args) => {
      const sinceMs =
        args.hours_back != null
          ? Date.now() - args.hours_back * 60 * 60 * 1000
          : undefined;
      const entries = await listActivity(ctx.tenantId, {
        kind: args.kind ?? undefined,
        sinceMs,
        searchSubstring: args.contains ?? undefined,
        limit: args.limit ?? 50,
      });
      const total = await countActivity(ctx.tenantId);
      return {
        ok: true,
        total,
        returned: entries.length,
        entries: entries.map((e) => ({
          id: e.id,
          ts_iso: new Date(e.ts).toISOString(),
          kind: e.kind,
          summary: e.summary,
          meta: e.meta,
        })),
      };
    },
  });
}
