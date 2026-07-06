// app/tools/zohoCliqTools.ts
//
// Agent tools for the first-party Zoho Cliq integration (app/lib/zohoCliq.ts).
// Cliq is not in Composio, so these are the sanctioned equivalents:
//   zoho_cliq_connect — mints the OAuth link and DELIVERS IT ITSELF (the model
//     never sees/retypes the URL — same anti-hallucination contract as
//     start_integration_auth)
//   zoho_cliq_status  — is Cliq connected for this tenant?
//   zoho_cliq_action  — execute a ZOHOCLIQ_* action (Composio-shaped registry)

import { tool } from "ai";
import { z } from "zod/v4";

import type { Channel } from "@/app/lib/identity";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import {
  zohoCliqAuthUrl,
  zohoCliqConnected,
  executeZohoCliqAction,
  ZOHO_CLIQ_ACTIONS,
} from "@/app/lib/zohoCliq";

export type ZohoToolContext = {
  tenantId: string;
  channel?: Channel;
  sessionId?: string;
};

export function makeZohoCliqConnectTool(ctx: ZohoToolContext) {
  return tool({
    description: [
      "Connect (or reconnect) the user's Zoho Cliq account via OAuth. This tool",
      "mints a fresh single-use link and SENDS it to the user's chat itself —",
      "you will not see the URL and must NEVER write or guess a Zoho link",
      "yourself. Use when zoho_cliq_status shows not connected, or a",
      "zoho_cliq_action returns status===401 with an error that is NOT about",
      "scope (a genuinely dead/revoked token). Do NOT use this for a 401 whose",
      "error mentions 'scope' — that means the Zoho API Console app",
      "registration itself isn't configured to allow that scope, which is an",
      "account-configuration problem on api-console.zoho.com; reconnecting",
      "issues the same under-scoped grant again and will not fix it. Also do",
      "not use this for any non-401 status — those are real action problems",
      "(bad chat_id, etc), not connection problems. If the user says the link",
      "expired, call this again for a fresh one.",
    ].join("\n"),
    inputSchema: z.object({}),
    execute: async () => {
      const out = await zohoCliqAuthUrl({
        tenantId: ctx.tenantId,
        channel: ctx.channel,
        sessionId: ctx.sessionId,
      });
      if (!out.ok) return { ok: false, error: out.error };
      if (ctx.channel && ctx.sessionId) {
        try {
          await sendOutboundRuntime({
            channel: ctx.channel,
            sessionId: ctx.sessionId,
            text:
              `🔗 Connect Zoho Cliq here (fresh link, expires in ~15 min):\n${out.url}\n` +
              `Tell me when you're done and I'll continue.`,
          });
          return {
            ok: true,
            delivered: true,
            message:
              "The exact Zoho Cliq connect link was ALREADY sent to the user's " +
              "chat. Do NOT write any URL yourself — tell them to use the link " +
              "above and say when they've finished.",
          };
        } catch {
          // fall through to verbatim relay
        }
      }
      return {
        ok: true,
        auth_url: out.url,
        message: `Relay this link EXACTLY as-is (verbatim, no reformatting): ${out.url}`,
      };
    },
  });
}

export function makeZohoCliqStatusTool(ctx: ZohoToolContext) {
  return tool({
    description:
      "Check whether Zoho Cliq is connected for this user/workspace. Returns " +
      "connected:true/false — if false, use zoho_cliq_connect. `granted_scope` " +
      "(when present) is what Zoho's OAuth server actually granted, which can " +
      "be NARROWER than what was requested if the app's registration on " +
      "Zoho's own API Console isn't configured to allow every requested " +
      "scope — that is an account-configuration gap on api-console.zoho.com, " +
      "not something zoho_cliq_connect can fix by itself; reconnecting will " +
      "not widen it. If a zoho_cliq_action later 401s with a scope error, " +
      "check this field and tell the user plainly which scope is missing " +
      "from their Zoho API Console app configuration rather than looping " +
      "reconnect attempts.",
    inputSchema: z.object({}),
    execute: async () => {
      const s = await zohoCliqConnected(ctx.tenantId);
      return {
        ok: true,
        connected: s.connected,
        ...(s.apiBase ? { api_region: s.apiBase } : {}),
        ...(s.connectedAt ? { connected_at: new Date(s.connectedAt).toISOString() } : {}),
        ...(s.grantedScope ? { granted_scope: s.grantedScope } : {}),
      };
    },
  });
}

export function makeZohoCliqActionTool(ctx: ZohoToolContext) {
  return tool({
    description: [
      "Execute a Zoho Cliq action for this user (Cliq is a first-party",
      "integration, NOT available through COMPOSIO_EXECUTE_TOOL — always use",
      "this tool for Cliq). Actions and their args:",
      ...Object.entries(ZOHO_CLIQ_ACTIONS).map(([k, v]) => `  ${k} — ${v}`),
      "ERROR HANDLING — read both `status` and `error` before deciding what to",
      "do next:",
      "  status===401 AND error mentions 'scope' → the Zoho API Console app",
      "  registration isn't configured to allow that scope. Call zoho_cliq_status",
      "  and read granted_scope, then tell the user PLAINLY which scope is",
      "  missing so they can add it in api-console.zoho.com — zoho_cliq_connect",
      "  will NOT fix this (it just re-grants the same restricted scope set),",
      "  do not loop it.",
      "  status===401 WITHOUT a scope-shaped error → genuinely dead/revoked",
      "  connection, use zoho_cliq_connect.",
      "  status is 400/403/404/anything else → this is a REAL problem with the",
      "  action itself (e.g. an invalid/unknown chat_id, missing permission on",
      "  that specific chat, bad args) — do NOT call zoho_cliq_connect, it will",
      "  NOT fix this. Tell the user the actual error instead.",
      "To message someone you have no existing chat_id for: there is no",
      "name/email lookup action. Try ZOHOCLIQ_LIST_CHATS first — it may already",
      "list an existing chat with them. If not, you need their numeric Zoho",
      "Cliq user_id from the user to call ZOHOCLIQ_CREATE_CHAT — do not guess one.",
    ].join("\n"),
    inputSchema: z.object({
      action: z
        .string()
        .min(1)
        .describe("Action slug, e.g. ZOHOCLIQ_SEND_MESSAGE."),
      args: z
        .string()
        .describe('JSON object string of the action arguments, e.g. {"chat_id":"...","text":"hi"}. Use "{}" for none.'),
    }),
    execute: async (input) => {
      let args: Record<string, unknown> = {};
      try {
        const parsed = JSON.parse(input.args || "{}");
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) args = parsed;
      } catch {
        return { ok: false, error: "args is not a valid JSON object string" };
      }
      return executeZohoCliqAction(ctx.tenantId, input.action, args);
    },
  });
}
