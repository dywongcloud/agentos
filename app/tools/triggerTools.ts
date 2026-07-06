// app/tools/triggerTools.ts
//
// Tools the agent calls to manage event subscriptions on the user's
// connected Composio integrations.
//
//   subscribe_to_trigger     — enable a trigger; the user starts receiving
//                              event notifications in Telegram.
//   unsubscribe_from_trigger — disable a previously-enabled trigger.
//   list_my_subscriptions    — what triggers this user currently has on.
//
// To discover WHICH triggers a toolkit supports, the agent should use the
// existing Composio meta tools (composio_api_request to /triggers_types/list)
// — we don't duplicate that wheel here.

import { tool } from "ai";
import { z } from "zod/v4";
import type { Channel } from "@/app/lib/identity";
import { sendOutboundRuntime } from "@/app/lib/outbound";

import {
  subscribeTrigger,
  unsubscribeTrigger,
  listSubscriptions,
} from "@/app/lib/composioTriggers";
import {
  initiateConnection,
  isToolkitConnected,
  listConnectedToolkits,
  listTriggerTypes,
} from "@/app/lib/composioConnections";
import {
  listCustomTriggerTypes,
  getCustomTriggerType,
  subscribeCustomTrigger,
  unsubscribeCustomTrigger,
  listCustomSubscriptions,
  registerCustomTriggerType,
  isCustomSubId,
} from "@/app/lib/customTriggers";

export type TriggerToolContext = {
  tenantId: string;
  // Base URL of this deployment, used to build the OAuth callback link.
  // Optional — Composio falls back to its own default callback if absent.
  baseUrl?: string;
  // Delivery target for links the model must NOT retype (auth URLs). When set,
  // start_integration_auth sends the exact minted URL to this chat itself.
  channel?: Channel;
  sessionId?: string;
};

export function makeSubscribeTriggerTool(ctx: TriggerToolContext) {
  return tool({
    description: [
      "Subscribe the user to a Composio trigger so they receive a Telegram",
      "message every time the event fires on their connected integration.",
      "",
      "Example flow:",
      "  1. User: 'tell me when someone opens a PR on my repo'",
      "  2. You: check user has GitHub connected (composio meta tools)",
      "  3. You: call subscribe_to_trigger with",
      "     slug='GITHUB_PULL_REQUEST_EVENT' and",
      "     trigger_config={ owner: 'foo', repo: 'bar' }",
      "  4. Composio will POST our webhook endpoint each time the event",
      "     fires; the user sees a Telegram notification.",
      "",
      "If you don't know the exact slug or the required config keys, call",
      "the Composio meta tools (composio_api_request /triggers_types/list)",
      "to inspect first.",
    ].join("\n"),
    inputSchema: z.object({
      slug: z
        .string()
        .min(1)
        .describe(
          "Composio trigger slug, e.g. 'GITHUB_PULL_REQUEST_EVENT', 'GMAIL_NEW_GMAIL_MESSAGE'."
        ),
      trigger_config: z
        .record(z.string(), z.unknown())
        .nullable()
        .describe(
          "Trigger-specific configuration (e.g. {owner, repo} for GitHub). Inspect via composio_api_request /triggers_types/{slug} if unsure."
        ),
      connected_account_id: z
        .string()
        .nullable()
        .describe(
          "Specific connected-account id to bind. Optional — defaults to the user's first matching connection."
        ),
    }),
    execute: async (args) => {
      // Custom (polling) trigger types live in our local registry — route
      // those to the local subscription store instead of Composio, which has
      // no native trigger for them (e.g. monday.com).
      const custom = await getCustomTriggerType(args.slug);
      if (custom) {
        const out = await subscribeCustomTrigger({
          tenantId: ctx.tenantId,
          slug: args.slug,
          config: args.trigger_config ?? undefined,
          connectedAccountId: args.connected_account_id ?? undefined,
        });
        if (!out.ok) return { ok: false, error: out.error };
        return {
          ok: true,
          trigger_id: out.subId,
          kind: "custom_polling",
          note: `Subscribed via local polling (${custom.toolkit} has no native Composio trigger). You'll get a chat message when it fires.`,
        };
      }
      const out = await subscribeTrigger({
        tenantId: ctx.tenantId,
        slug: args.slug,
        triggerConfig: args.trigger_config ?? undefined,
        connectedAccountId: args.connected_account_id ?? undefined,
      });
      if (!out.ok) return { ok: false, error: out.error };
      return { ok: true, trigger_id: out.triggerId };
    },
  });
}

export function makeUnsubscribeTriggerTool(ctx: TriggerToolContext) {
  return tool({
    description:
      "Unsubscribe / disable a trigger by its instance id. Call list_my_subscriptions first if you don't know the id.",
    inputSchema: z.object({
      trigger_id: z.string().min(1).describe("Composio trigger instance id."),
    }),
    execute: async (args) => {
      // Local custom-trigger subscriptions use "cst_" ids.
      if (isCustomSubId(args.trigger_id)) {
        return await unsubscribeCustomTrigger(args.trigger_id);
      }
      const out = await unsubscribeTrigger(args.trigger_id);
      // ctx referenced for symmetry with subscribe — keep tenant scoping
      // visible to future readers even though Composio's API is global.
      void ctx;
      return out;
    },
  });
}

export function makeListSubscriptionsTool(ctx: TriggerToolContext) {
  return tool({
    description:
      "List the user's active Composio trigger subscriptions (what events they're getting notified about).",
    inputSchema: z.object({}),
    execute: async () => {
      const [subs, custom] = await Promise.all([
        listSubscriptions(ctx.tenantId),
        listCustomSubscriptions(ctx.tenantId),
      ]);
      const composioSubs = subs.map((s) => ({
        trigger_id: s.triggerId,
        trigger_name: s.triggerName,
        connected_account_id: s.connectedAccountId,
        kind: "composio" as const,
      }));
      const customSubs = custom.map((s) => ({
        trigger_id: s.subId,
        trigger_name: s.slug,
        connected_account_id: s.connectedAccountId ?? null,
        kind: "custom_polling" as const,
        config: s.config,
      }));
      const all = [...composioSubs, ...customSubs];
      return { ok: true, count: all.length, subscriptions: all };
    },
  });
}

// Discover available triggers for a toolkit (and/or by keyword). This is the
// FIRST step in the natural-language subscribe flow — the agent calls it to
// resolve a fuzzy user request ("ping me about new emails") into a concrete
// trigger slug + its config schema, without the user ever typing a command.
export function makeDiscoverTriggersTool(ctx: TriggerToolContext) {
  return tool({
    description: [
      "Discover Composio triggers the user can subscribe to. Use this the",
      "moment a user expresses interest in being notified about something",
      "('let me know when…', 'ping me if…', 'tell me about new…'). You don't",
      "need a command — infer the toolkit from what they said.",
      "",
      "Pass `toolkit` (e.g. 'gmail', 'slack', 'github', 'notion', 'linear')",
      "when you can guess it, and/or `keyword` to narrow within a toolkit",
      "('message', 'pull request', 'new row'). Returns slugs + config schemas",
      "so you can call subscribe_to_trigger next.",
    ].join("\n"),
    inputSchema: z.object({
      toolkit: z
        .string()
        .nullable()
        .describe(
          "Toolkit slug to scope discovery to, e.g. 'gmail', 'slack', 'github'. null to search broadly (slower; prefer naming one)."
        ),
      keyword: z
        .string()
        .nullable()
        .describe(
          "Keyword to rank/filter triggers within the toolkit, e.g. 'new email', 'pull request', 'reaction'."
        ),
    }),
    execute: async (args) => {
      const triggers = await listTriggerTypes({
        toolkits: args.toolkit ? [args.toolkit] : undefined,
        keyword: args.keyword ?? undefined,
        limit: 12,
      });
      const native = triggers.map((t) => ({
        slug: t.slug,
        name: t.name,
        description: t.description.slice(0, 300),
        toolkit: t.toolkitSlug,
        config_schema: t.configSchema,
        kind: "composio" as const,
      }));

      // Merge our local custom (polling) trigger types — these cover toolkits
      // Composio has no native triggers for (e.g. monday.com). Filter by the
      // requested toolkit and optional keyword so they rank alongside natives.
      const kw = (args.keyword ?? "").toLowerCase().trim();
      let customTypes = await listCustomTriggerTypes(args.toolkit ?? undefined);
      if (kw) {
        customTypes = customTypes.filter(
          (t) =>
            t.slug.toLowerCase().includes(kw) ||
            t.name.toLowerCase().includes(kw) ||
            t.description.toLowerCase().includes(kw)
        );
      }
      const custom = customTypes.map((t) => ({
        slug: t.slug,
        name: t.name,
        description: t.description.slice(0, 300),
        toolkit: t.toolkit,
        config_schema: t.configSchema,
        kind: "custom_polling" as const,
      }));

      const all = [...native, ...custom];
      return {
        ok: true,
        count: all.length,
        triggers: all,
        hint:
          all.length === 0
            ? "No triggers matched. Try a different toolkit slug or drop the keyword. For an app with no built-in triggers, you can mint one with register_custom_trigger."
            : "Pick the best slug, confirm the toolkit is connected (check_integration_connected), then call subscribe_to_trigger. Slugs marked kind=custom_polling are delivered by our local poller.",
      };
    },
  });
}

// Check whether the user already has an ACTIVE connection for a toolkit. The
// agent uses this before subscribing so it can route the user through auth if
// the toolkit isn't connected yet.
export function makeCheckIntegrationTool(ctx: TriggerToolContext) {
  return tool({
    description: [
      "Check whether the user already connected a given toolkit (gmail,",
      "slack, github, …). Call this before subscribe_to_trigger so you know",
      "whether to subscribe directly or first send them through auth via",
      "start_integration_auth.",
      "Pass no toolkit to list everything they've connected.",
    ].join("\n"),
    inputSchema: z.object({
      toolkit: z
        .string()
        .nullable()
        .describe("Toolkit slug to check, e.g. 'gmail'. null to list all connections."),
    }),
    execute: async (args) => {
      if (!args.toolkit) {
        const all = await listConnectedToolkits(ctx.tenantId);
        return {
          ok: true,
          connected_toolkits: all.map((c) => ({
            toolkit: c.toolkitSlug,
            status: c.status,
            account_id: c.connectedAccountId,
          })),
        };
      }
      const res = await isToolkitConnected(ctx.tenantId, args.toolkit);
      // Could not verify (Composio API errored after retries). This is NOT the
      // same as "not connected" — do not let the model invent an expiry.
      if (res.error) {
        return {
          ok: false,
          toolkit: args.toolkit,
          connected: null,
          verify_error: true,
          error: res.errorMessage ?? "could not verify connection",
          hint:
            "Could NOT verify the connection due to a temporary Composio API " +
            "error — this does NOT mean it expired or was disconnected. Do NOT " +
            "tell the user their connection expired or ask them to reconnect. " +
            "Either retry check_integration_connected once, or just proceed and " +
            "attempt the action; only surface a reconnect prompt if the actual " +
            "tool call returns a real auth/expired error.",
        };
      }
      return {
        ok: true,
        toolkit: args.toolkit,
        connected: res.connected,
        status: res.status ?? null,
        account_id: res.accountId ?? null,
        matched_slug: res.matchedSlug ?? null,
        // Connected but Composio labels the record EXPIRED/INACTIVE. It almost
        // always still executes (token refreshes on use), so attempt the action
        // rather than preemptively prompting a reconnect.
        ...(res.connected && res.stale
          ? {
              stale: true,
              hint:
                `A '${args.toolkit}' account is connected but Composio currently ` +
                `labels it '${res.status}'. Composio refreshes these on use, so ` +
                `PROCEED and attempt the action normally. Do NOT tell the user it ` +
                `expired and do NOT call start_integration_auth yet — only do that ` +
                `if the actual COMPOSIO_EXECUTE_TOOL call returns a real 401/403/` +
                `not-connected error.`,
            }
          : {}),
        // When not connected, list what the user HAS connected so the model can
        // recognize it used the wrong slug instead of falsely starting auth.
        ...(res.connected
          ? {}
          : {
              also_connected: res.alsoConnected ?? [],
              hint: (res.alsoConnected ?? []).length
                ? `Not matched as '${args.toolkit}'. The user DOES have these connected: ${(res.alsoConnected ?? []).join(", ")}. If one of these is the toolkit you meant, use that exact slug and do NOT start auth.`
                : "No '" + args.toolkit + "' account exists at all (the API call succeeded), so the user genuinely has not connected this app. Starting auth is appropriate.",
            }),
      };
    },
  });
}

// Start an OAuth (or other) connection flow for a toolkit the user hasn't
// connected yet. CRITICAL anti-hallucination design: the freshly-minted auth
// URL is delivered to the user's chat BY THIS TOOL, byte-for-byte — the model
// never sees or retypes it. Model-retyped links were arriving mangled
// (truncated params) or replaced with remembered generic app.composio.dev
// URLs, and old links re-sent from chat history had already expired.
export function makeStartIntegrationAuthTool(ctx: TriggerToolContext) {
  return tool({
    description: [
      "Begin connecting a toolkit the user hasn't authorized yet. This tool",
      "MINTS a fresh single-use auth link and SENDS it to the user's chat",
      "itself — you will not see the URL and must NEVER write, guess, or reuse",
      "a connect/auth URL yourself (links from memory or earlier turns are",
      "wrong or expired). This is the ONLY way to give the user a connect",
      "link; do not use COMPOSIO_MANAGE_CONNECTIONS for that. If the user says",
      "a link didn't work or expired, call this tool AGAIN for a fresh one.",
      "After they say they're done, re-check with check_integration_connected",
      "and then continue. Use only when check_integration_connected showed the",
      "toolkit is NOT connected (or a live call returned a real auth error).",
    ].join("\n"),
    inputSchema: z.object({
      toolkit: z
        .string()
        .min(1)
        .describe("Toolkit slug to connect, e.g. 'gmail', 'slack', 'github'."),
    }),
    execute: async (args) => {
      const out = await initiateConnection({
        tenantId: ctx.tenantId,
        toolkitSlug: args.toolkit,
        callbackUrl: ctx.baseUrl
          ? `${ctx.baseUrl.replace(/\/$/, "")}/api/claw?op=health`
          : undefined,
      });
      if (!out.ok) return { ok: false, error: out.error };

      // Preferred path: deliver the exact URL straight to the chat so the model
      // can't mangle it. Only fall back to handing the model the URL when we
      // have no delivery target (e.g. a background context with no session).
      if (ctx.channel && ctx.sessionId) {
        try {
          await sendOutboundRuntime({
            channel: ctx.channel,
            sessionId: ctx.sessionId,
            text:
              `🔗 Connect ${args.toolkit} here (fresh link, expires soon):\n` +
              `${out.authUrl}\n` +
              `Tell me when you're done and I'll continue.`,
          });
          return {
            ok: true,
            delivered: true,
            message:
              `The exact ${args.toolkit} connect link was ALREADY sent to the ` +
              `user's chat. Do NOT write any URL yourself — just tell them to ` +
              `use the link above and to say when they've finished.`,
          };
        } catch {
          // delivery failed — fall through to returning the URL verbatim
        }
      }
      return {
        ok: true,
        auth_url: out.authUrl,
        message:
          `Relay this link to the user EXACTLY as-is (copy verbatim, do not ` +
          `shorten, re-format, or substitute another URL), then re-check the ` +
          `connection: ${out.authUrl}`,
      };
    },
  });
}

// Mint a brand-new custom (polling) trigger TYPE for any toolkit — including
// apps Composio has no native triggers for. The new slug immediately becomes
// discoverable + subscribable like any built-in. Use this when discover_triggers
// finds nothing native and the user wants to be notified about an arbitrary
// condition (e.g. "a monday deal moving to the Won stage").
//
// The trigger works by polling a Composio READ action on a timer and firing
// when a NEW record appears (deduped by id) that passes the optional match
// filters. You must know the action's response shape well enough to point
// `items_path` at the array of records; if unsure, leave it null and the poller
// deep-searches for the largest array of objects.
export function makeRegisterCustomTriggerTool(ctx: TriggerToolContext) {
  return tool({
    description: [
      "Create a NEW custom polling trigger type for a toolkit that has no",
      "native Composio trigger (e.g. monday.com), or for any arbitrary",
      "condition the user wants to watch. After registering, subscribe the",
      "user with subscribe_to_trigger(slug, trigger_config).",
      "",
      "How it works: every couple of minutes we run `poll_action` (a Composio",
      "READ action) with the subscriber's config as arguments, pull the array",
      "of records, and fire a chat notification for each NEW record (deduped by",
      "id) that passes `match`. Use COMPOSIO_GET_TOOL_SCHEMAS on the action",
      "first to learn its exact argument + response shape.",
    ].join("\n"),
    inputSchema: z.object({
      slug: z
        .string()
        .regex(/^[A-Z0-9_]+$/)
        .describe("UPPER_SNAKE_CASE slug for the new trigger, e.g. 'MONDAY_DEAL_WON'."),
      toolkit: z.string().min(1).describe("Toolkit slug, e.g. 'monday'."),
      name: z.string().min(1).describe("Short human name shown in discovery."),
      description: z.string().min(1).describe("What this trigger fires on."),
      poll_action: z
        .string()
        .min(1)
        .describe("Composio READ action slug to poll, e.g. 'MONDAY_GET_ACTIVITY_LOGS'."),
      config_fields: z
        .array(
          z.object({
            key: z.string(),
            type: z.string().default("string"),
            description: z.string().nullable(),
            required: z.boolean().default(true),
          })
        )
        .describe(
          "Config the subscriber must supply (also passed as action arguments), e.g. [{key:'board_id', required:true}]."
        ),
      base_args: z
        .record(z.string(), z.unknown())
        .nullable()
        .describe("Static arguments always passed to the action (merged under config)."),
      since_arg: z
        .string()
        .nullable()
        .describe("Action arg name to receive the last-poll ISO timestamp (e.g. 'from'), if the action supports time filtering."),
      items_path: z
        .string()
        .nullable()
        .describe("Dot-path to the records array in the action response, e.g. 'data.activity_logs'. null = auto-detect."),
      id_path: z
        .string()
        .nullable()
        .describe("Dot-path to a stable id per record for dedupe, e.g. 'id'. null = auto."),
      match: z
        .array(
          z.object({
            path: z.string(),
            equals: z.string().nullable(),
            contains: z.string().nullable(),
          })
        )
        .nullable()
        .describe("Optional filters; ALL must pass for a record to fire."),
      title_template: z
        .string()
        .nullable()
        .describe("Notification text; {{field}} from the record, {{config.key}} from config."),
    }),
    execute: async (args) => {
      const properties: Record<string, { type: string; description?: string }> = {};
      const required: string[] = [];
      for (const f of args.config_fields) {
        properties[f.key] = {
          type: f.type || "string",
          ...(f.description ? { description: f.description } : {}),
        };
        if (f.required) required.push(f.key);
      }
      const out = await registerCustomTriggerType({
        tenantId: ctx.tenantId,
        type: {
          slug: args.slug,
          toolkit: args.toolkit,
          name: args.name,
          description: args.description,
          configSchema: { type: "object", properties, required },
          poll: {
            action: args.poll_action,
            baseArgs: args.base_args ?? undefined,
            sinceArg: args.since_arg ?? undefined,
            itemsPaths: args.items_path ? [args.items_path] : undefined,
            idPath: args.id_path ?? undefined,
            match:
              args.match?.map((m) => ({
                path: m.path,
                equals: m.equals ?? undefined,
                contains: m.contains ?? undefined,
              })) ?? undefined,
            titleTemplate: args.title_template ?? undefined,
          },
        },
      });
      if (!out.ok) return { ok: false, error: out.error };
      return {
        ok: true,
        slug: out.slug,
        message: `Registered custom trigger ${out.slug}. Now call subscribe_to_trigger('${out.slug}', { ${required.join(", ")} }) to turn it on.`,
      };
    },
  });
}
