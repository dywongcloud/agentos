// app/lib/customTriggers.ts
//
// Local "custom trigger" layer — a polling shim that gives users subscribable
// triggers for toolkits Composio has NO native triggers for (e.g. monday.com,
// which exposes 0 Composio triggers). Composio's SDK can only create instances
// of trigger types it already knows; it cannot register brand-new trigger
// TYPES. So instead of webhooks we poll a toolkit's read actions on a timer,
// diff against last-seen state, and deliver new/matching events to the user's
// chat through the SAME path the real Composio webhook uses.
//
// Two sources of trigger TYPES:
//   1. Built-ins defined in code (the monday.com set below).
//   2. Dynamically registered custom types (Redis hash `ctrig:types`) so the
//      agent can mint arbitrary polling triggers from chat — e.g. "tell me when
//      a deal moves to the Won stage" → a custom type that polls an action and
//      fires only when a column value matches.
//
// Everything is keyed off Composio action execution (composio.tools.execute),
// so a custom type just needs: which action to poll, how to pull the array of
// candidate records out of the response, how to identify each record (dedupe),
// and an optional match filter + title template.

import { Composio } from "@composio/core";

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import { isToolkitConnected } from "@/app/lib/composioConnections";
import { getSessionMeta, getLastSession } from "@/app/lib/sessionMeta";
import { teamGroupSession } from "@/app/lib/teams";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { recordActivity } from "@/app/lib/activityLog";
import type { Channel } from "@/app/lib/identity";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type CustomTriggerMatch = {
  // Dot-path into a candidate record, e.g. "event" or "column_values.0.text".
  path: string;
  equals?: string; // case-insensitive exact match
  contains?: string; // case-insensitive substring
};

export type CustomTriggerPoll = {
  // Action slug to execute on each poll — a Composio slug (e.g.
  // "MONDAY_GET_ACTIVITY_LOGS") or a first-party one (ZOHOCLIQ_* routes to our
  // own Zoho Cliq executor; see executeAction below).
  action: string;
  // Static base args merged into the action call (subscription config is merged
  // on top of these, so config can override).
  baseArgs?: Record<string, unknown>;
  // If set, the last-poll timestamp is passed under this arg name so the
  // action only returns recent records (e.g. "from" for activity logs).
  sinceArg?: string;
  // Timestamp format for sinceArg: ISO string (default) or epoch millis
  // ("ms" — what Zoho Cliq's fromtime expects).
  sinceFormat?: "iso" | "ms";
  // Ordered candidate dot-paths to the array of records in the response. The
  // first that resolves to a non-empty array wins; if none do, we deep-search
  // for the largest array of objects.
  itemsPaths?: string[];
  // Dot-path to a stable id within each record (for dedupe). Falls back to
  // common id keys, then a hash of the record.
  idPath?: string;
  // Optional filters — ALL must pass for a record to fire.
  match?: CustomTriggerMatch[];
  // Human title template; {{path}} pulls from the record, {{config.x}} from the
  // subscription config.
  titleTemplate?: string;
};

export type CustomTriggerType = {
  slug: string;
  toolkit: string;
  name: string;
  description: string;
  // JSON-schema-ish object describing the subscription config the agent must
  // collect (e.g. { board_id }). Surfaced to the agent via discover_triggers.
  configSchema: {
    type: "object";
    properties: Record<string, { type: string; description?: string }>;
    required?: string[];
  };
  poll: CustomTriggerPoll;
  minIntervalSec?: number; // don't poll more often than this (default 120)
  builtin?: boolean;
  createdBy?: string;
  createdAt?: number;
};

export type CustomTriggerSubscription = {
  subId: string; // our id, "cst_..."
  tenantId: string;
  slug: string;
  config: Record<string, unknown>;
  connectedAccountId?: string;
  createdAt: number;
  lastPolledAt?: number;
};

// ---------------------------------------------------------------------------
// Built-in monday.com trigger types
// ---------------------------------------------------------------------------

const MONDAY_BOARD_CONFIG = {
  type: "object" as const,
  properties: {
    board_id: {
      type: "string",
      description:
        "monday.com board id to watch (numeric id from the board URL).",
    },
  },
  required: ["board_id"],
};

const BUILTIN_TYPES: CustomTriggerType[] = [
  {
    slug: "MONDAY_BOARD_ACTIVITY",
    toolkit: "monday",
    name: "monday.com — any board activity",
    description:
      "Fires on ANY activity-log entry on a watched board since the last check (item created, column changed, etc.). Broadest monday trigger.",
    configSchema: MONDAY_BOARD_CONFIG,
    poll: {
      action: "MONDAY_GET_ACTIVITY_LOGS",
      sinceArg: "from",
      itemsPaths: [
        "data.activity_logs",
        "activity_logs",
        "data.data.boards.0.activity_logs",
        "data.boards.0.activity_logs",
      ],
      idPath: "id",
      titleTemplate: "monday board activity: {{event}}",
    },
    minIntervalSec: 60,
    builtin: true,
  },
  {
    slug: "MONDAY_NEW_ITEM",
    toolkit: "monday",
    name: "monday.com — new item created",
    description:
      "Fires when a new item/row is created on a watched board (matches create_pulse activity).",
    configSchema: MONDAY_BOARD_CONFIG,
    poll: {
      action: "MONDAY_GET_ACTIVITY_LOGS",
      sinceArg: "from",
      itemsPaths: [
        "data.activity_logs",
        "activity_logs",
        "data.data.boards.0.activity_logs",
        "data.boards.0.activity_logs",
      ],
      idPath: "id",
      match: [{ path: "event", contains: "create_pulse" }],
      titleTemplate: "New monday item created on board {{config.board_id}}",
    },
    minIntervalSec: 60,
    builtin: true,
  },
  {
    slug: "MONDAY_ITEM_UPDATED",
    toolkit: "monday",
    name: "monday.com — item updated / status changed",
    description:
      "Fires when an item's column or status changes on a watched board (matches column/status update activity).",
    configSchema: MONDAY_BOARD_CONFIG,
    poll: {
      action: "MONDAY_GET_ACTIVITY_LOGS",
      sinceArg: "from",
      itemsPaths: [
        "data.activity_logs",
        "activity_logs",
        "data.data.boards.0.activity_logs",
        "data.boards.0.activity_logs",
      ],
      idPath: "id",
      match: [{ path: "event", contains: "column" }],
      titleTemplate: "monday item updated ({{event}}) on board {{config.board_id}}",
    },
    minIntervalSec: 60,
    builtin: true,
  },
  {
    slug: "MONDAY_NEW_UPDATE",
    toolkit: "monday",
    name: "monday.com — new update/comment",
    description:
      "Fires when someone posts a new update/comment on items of a watched board.",
    configSchema: MONDAY_BOARD_CONFIG,
    // MONDAY_GET_UPDATES is item-scoped (requires item_id), so a board-level
    // subscription can never satisfy it — every poll failed with a missing
    // item_id error and looked like an expired connection. Read the board's
    // activity log instead and match the create_update event.
    poll: {
      action: "MONDAY_GET_ACTIVITY_LOGS",
      sinceArg: "from",
      itemsPaths: [
        "data.activity_logs",
        "activity_logs",
        "data.data.boards.0.activity_logs",
        "data.boards.0.activity_logs",
      ],
      idPath: "id",
      match: [{ path: "event", contains: "create_update" }],
      titleTemplate: "New monday update/comment on board {{config.board_id}}",
    },
    minIntervalSec: 60,
    builtin: true,
  },
  // ── Zoho Cliq (first-party integration — polls via our own executor) ──────
  {
    slug: "ZOHOCLIQ_NEW_MESSAGE",
    toolkit: "zohocliq",
    name: "Zoho Cliq — new message in a chat/channel",
    description:
      "Fires when a new message lands in a watched Zoho Cliq chat or channel (polls the chat's messages since the last check).",
    configSchema: {
      type: "object",
      properties: {
        chat_id: {
          type: "string",
          description: "Zoho Cliq chat/channel id to watch (from ZOHOCLIQ_LIST_CHATS).",
        },
      },
      required: ["chat_id"],
    },
    poll: {
      action: "ZOHOCLIQ_LIST_MESSAGES",
      sinceArg: "fromtime",
      sinceFormat: "ms",
      itemsPaths: ["data.data", "data.messages", "data", "messages"],
      idPath: "id",
      titleTemplate: "New Cliq message in chat {{config.chat_id}}",
    },
    minIntervalSec: 60,
    builtin: true,
  },
  {
    slug: "ZOHOCLIQ_NEW_CHAT",
    toolkit: "zohocliq",
    name: "Zoho Cliq — new chat/conversation",
    description:
      "Fires when a chat that wasn't there before appears in the user's Cliq chat list (new DM, group or channel conversation).",
    configSchema: { type: "object", properties: {} },
    poll: {
      action: "ZOHOCLIQ_LIST_CHATS",
      itemsPaths: ["data.data", "data.chats", "data", "chats"],
      idPath: "chat_id",
      titleTemplate: "New Cliq conversation: {{title}}{{name}}",
    },
    minIntervalSec: 120,
    builtin: true,
  },
  {
    slug: "ZOHOCLIQ_NEW_PINNED_MESSAGE",
    toolkit: "zohocliq",
    name: "Zoho Cliq — message pinned in a chat",
    description:
      "Fires when a new message is pinned in a watched Zoho Cliq chat/channel.",
    configSchema: {
      type: "object",
      properties: {
        chat_id: {
          type: "string",
          description: "Zoho Cliq chat/channel id to watch for new pins.",
        },
      },
      required: ["chat_id"],
    },
    poll: {
      action: "ZOHOCLIQ_LIST_PINNED_MESSAGES",
      itemsPaths: ["data.data", "data.pinmessages", "data", "pinmessages"],
      idPath: "id",
      titleTemplate: "New pinned message in Cliq chat {{config.chat_id}}",
    },
    minIntervalSec: 120,
    builtin: true,
  },
];

// ---------------------------------------------------------------------------
// Redis keys
// ---------------------------------------------------------------------------

const K_TYPES = "ctrig:types"; // hash: slug -> JSON CustomTriggerType (custom only)
const K_SUB = (subId: string) => `ctrig:sub:${subId}`;
const K_SUBS_INDEX = "ctrig:subs:index"; // set of all subIds
const K_SUBS_TENANT = (tenantId: string) => `ctrig:subs:t:${tenantId}`;
const K_SEEN = (subId: string) => `ctrig:seen:${subId}`; // JSON array of seen ids (bounded)

const SEEN_CAP = 500;

// ---------------------------------------------------------------------------
// Type registry
// ---------------------------------------------------------------------------

export function isCustomSubId(id: string): boolean {
  return typeof id === "string" && id.startsWith("cst_");
}

// Connection probe that understands FIRST-PARTY toolkits too: "zohocliq" is
// checked against our own token store; everything else goes through Composio.
async function toolkitConnectedAny(
  tenantId: string,
  toolkit: string
): Promise<{ connected: boolean; accountId?: string; status?: string }> {
  if (toolkit.toLowerCase().replace(/[^a-z0-9]/g, "") === "zohocliq") {
    const { zohoCliqConnected } = await import("@/app/lib/zohoCliq");
    const s = await zohoCliqConnected(tenantId);
    return { connected: s.connected, status: s.connected ? "ACTIVE" : "DISCONNECTED" };
  }
  return isToolkitConnected(tenantId, toolkit);
}

export async function listCustomTriggerTypes(
  toolkit?: string
): Promise<CustomTriggerType[]> {
  const store = getStore();
  let custom: CustomTriggerType[] = [];
  try {
    const all = await store.get<Record<string, CustomTriggerType>>(K_TYPES);
    // Stored as a JSON object map under one key for simplicity.
    if (all && typeof all === "object") custom = Object.values(all);
  } catch {
    custom = [];
  }
  const merged = [...BUILTIN_TYPES, ...custom];
  if (!toolkit) return merged;
  const tk = toolkit.toLowerCase().replace(/[^a-z0-9]/g, "");
  return merged.filter(
    (t) => t.toolkit.toLowerCase().replace(/[^a-z0-9]/g, "") === tk
  );
}

export async function getCustomTriggerType(
  slug: string
): Promise<CustomTriggerType | null> {
  const all = await listCustomTriggerTypes();
  return all.find((t) => t.slug.toLowerCase() === slug.toLowerCase()) ?? null;
}

export async function registerCustomTriggerType(args: {
  tenantId: string;
  type: Omit<CustomTriggerType, "builtin" | "createdBy" | "createdAt">;
}): Promise<{ ok: true; slug: string } | { ok: false; error: string }> {
  const t = args.type;
  if (!t.slug || !/^[A-Z0-9_]+$/.test(t.slug)) {
    return {
      ok: false,
      error: "slug must be UPPER_SNAKE_CASE (A-Z, 0-9, underscore).",
    };
  }
  if (BUILTIN_TYPES.some((b) => b.slug === t.slug)) {
    return { ok: false, error: `slug ${t.slug} is a reserved built-in.` };
  }
  if (!t.poll?.action) {
    return { ok: false, error: "poll.action (a Composio action slug) is required." };
  }
  const store = getStore();
  const all =
    (await store.get<Record<string, CustomTriggerType>>(K_TYPES)) ?? {};
  all[t.slug] = {
    ...t,
    builtin: false,
    createdBy: args.tenantId,
    createdAt: Date.now(),
  };
  await store.set(K_TYPES, all);
  return { ok: true, slug: t.slug };
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

function newSubId(): string {
  return `cst_${Math.random().toString(36).slice(2, 10)}${Date.now().toString(36).slice(-4)}`;
}

export async function subscribeCustomTrigger(args: {
  tenantId: string;
  slug: string;
  config?: Record<string, unknown>;
  connectedAccountId?: string;
}): Promise<{ ok: true; subId: string } | { ok: false; error: string }> {
  const type = await getCustomTriggerType(args.slug);
  if (!type) return { ok: false, error: `unknown custom trigger slug ${args.slug}` };

  // Validate required config keys.
  const missing = (type.configSchema.required ?? []).filter(
    (k) => args.config?.[k] === undefined || args.config?.[k] === ""
  );
  if (missing.length) {
    return {
      ok: false,
      error: `missing required config: ${missing.join(", ")}`,
    };
  }

  // The poll runs a Composio action on `type.toolkit`. We look up the toolkit's
  // connected account to bind a concrete (preferably ACTIVE) account id so the
  // poll never picks a stale duplicate. We deliberately do NOT refuse the
  // subscription when the toolkit looks unconnected: Composio routinely
  // mislabels live connections as EXPIRED and its list endpoint occasionally
  // errors transiently, producing false negatives. Subscribe optimistically and
  // let the first real poll's execute call be the source of truth for auth —
  // the rule still gets created "nonetheless" rather than surfacing a scary
  // "not connected" wall for a connection the user knows is live.
  const conn = await toolkitConnectedAny(args.tenantId, type.toolkit);

  const sub: CustomTriggerSubscription = {
    subId: newSubId(),
    tenantId: args.tenantId,
    slug: type.slug,
    config: args.config ?? {},
    connectedAccountId: args.connectedAccountId ?? conn.accountId,
    createdAt: Date.now(),
  };
  const store = getStore();
  await store.set(K_SUB(sub.subId), sub);
  await store.sadd(K_SUBS_INDEX, sub.subId);
  await store.sadd(K_SUBS_TENANT(args.tenantId), sub.subId);
  // Seed the seen-set on first poll (don't fire on pre-existing history) — we
  // mark lastPolledAt at creation so the first poll uses `from = now`.
  sub.lastPolledAt = Date.now();
  await store.set(K_SUB(sub.subId), sub);
  return { ok: true, subId: sub.subId };
}

export async function unsubscribeCustomTrigger(
  subId: string
): Promise<{ ok: boolean; error?: string }> {
  const store = getStore();
  const sub = await store.get<CustomTriggerSubscription>(K_SUB(subId));
  if (!sub) return { ok: false, error: "subscription not found" };
  await store.del(K_SUB(subId));
  await store.del(K_SEEN(subId));
  await store.srem(K_SUBS_INDEX, subId);
  await store.srem(K_SUBS_TENANT(sub.tenantId), subId);
  return { ok: true };
}

export async function listCustomSubscriptions(
  tenantId: string
): Promise<CustomTriggerSubscription[]> {
  const store = getStore();
  const ids = await store.smembers(K_SUBS_TENANT(tenantId));
  const out: CustomTriggerSubscription[] = [];
  for (const id of ids) {
    const sub = await store.get<CustomTriggerSubscription>(K_SUB(id));
    if (sub) out.push(sub);
  }
  return out;
}

// ---------------------------------------------------------------------------
// Polling helpers
// ---------------------------------------------------------------------------

// Composio frequently returns an action's payload as a JSON *string* (e.g.
// monday's MONDAY_GET_ACTIVITY_LOGS comes back as data:"{\"activity_logs\":[…]}"
// when there are results, but as a parsed object when empty). Walking such a
// value as an object yields nothing, so triggers silently never fire. Coerce a
// JSON-looking string back into structured data before/while traversing.
function coerceJson(v: unknown): unknown {
  if (typeof v !== "string") return v;
  const t = v.trim();
  if (t.length > 1 && (t[0] === "{" || t[0] === "[")) {
    try {
      return JSON.parse(t);
    } catch {
      return v;
    }
  }
  return v;
}

function getByPath(obj: unknown, path: string): unknown {
  if (!path) return undefined;
  let cur: any = coerceJson(obj);
  for (const seg of path.split(".")) {
    if (cur == null || typeof cur !== "object") return undefined;
    cur = coerceJson(cur[seg]);
  }
  return cur;
}

// Deep-search for the largest array of plain objects — fallback when no
// configured itemsPath resolves (Composio response shapes vary by action).
function deepFindItemArray(obj: unknown): unknown[] {
  let best: unknown[] = [];
  const visit = (v: unknown, depth: number) => {
    if (depth > 6 || v == null) return;
    if (Array.isArray(v)) {
      if (v.length > best.length && v.every((x) => x && typeof x === "object")) {
        best = v;
      }
      for (const x of v) visit(x, depth + 1);
      return;
    }
    if (typeof v === "object") {
      for (const k of Object.keys(v as Record<string, unknown>)) {
        visit((v as Record<string, unknown>)[k], depth + 1);
      }
    }
  };
  visit(obj, 0);
  return best;
}

function extractItems(resp: unknown, paths?: string[]): unknown[] {
  const root = coerceJson(resp);
  for (const p of paths ?? []) {
    const v = getByPath(root, p);
    if (Array.isArray(v) && v.length) return v;
  }
  return deepFindItemArray(root);
}

// Recursively parse JSON-string fields so a model (or the deterministic
// fallback) sees fully-structured data. monday activity-log entries nest the
// interesting bits under a `data` field that is itself a JSON string, e.g.
// {"column_id":"status","value":{"label":...},"previous_value":{...}}.
function deepCoerce(v: unknown, depth = 0): unknown {
  if (depth > 6) return v;
  const c = coerceJson(v);
  if (Array.isArray(c)) return c.map((x) => deepCoerce(x, depth + 1));
  if (c && typeof c === "object") {
    const out: Record<string, unknown> = {};
    for (const k of Object.keys(c as Record<string, unknown>)) {
      out[k] = deepCoerce((c as Record<string, unknown>)[k], depth + 1);
    }
    return out;
  }
  return c;
}

// Produce a human-readable, AI-written one/two-liner explaining what actually
// changed in a fired event — not just the event name. Falls back to null on any
// failure so the caller can use the static template instead.
async function summarizeChangeAI(
  type: CustomTriggerType,
  item: unknown,
  config: Record<string, unknown>
): Promise<string | null> {
  try {
    const { generateText } = await import("ai");
    const { buildLlmArgs } = await import("@/app/lib/modelRouting");
    const structured = deepCoerce(item);
    const payload = JSON.stringify(structured, null, 2).slice(0, 4000);
    const ctx = JSON.stringify(config).slice(0, 500);
    const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0.2 });
    const result = await generateText({
      ...llm,
      system: [
        `You write short ${type.toolkit} change notifications for a Telegram bot.`,
        "Given ONE raw activity/event record, explain in plain English what",
        "specifically changed — name the item, the field/column, and the",
        "before→after values when present (e.g. status moved \"In Progress\" →",
        "\"Done\"). For a new item/comment, say what was created and by whom if",
        "the record says. 1-2 sentences, concrete, no JSON, no buzzwords. If",
        "the record lacks detail, describe what you can infer plainly.",
      ].join("\n"),
      prompt:
        `Trigger: ${type.name} (${type.slug})\nConfig: ${ctx}\n\nRecord:\n${payload}`,
    });
    const text = result.text.trim();
    return text || null;
  } catch (err: any) {
    console.warn(
      `[customTriggers] AI summary failed for ${type.slug}: ${err?.message ?? String(err)}`
    );
    return null;
  }
}

function hashString(s: string): string {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = (h * 33) ^ s.charCodeAt(i);
  return (h >>> 0).toString(36);
}

function itemId(item: unknown, idPath?: string): string {
  const direct = idPath ? getByPath(item, idPath) : undefined;
  if (direct != null) return String(direct);
  const obj = (item ?? {}) as Record<string, unknown>;
  for (const k of ["id", "pulse_id", "item_id", "event_id", "uid"]) {
    if (obj[k] != null) return String(obj[k]);
  }
  return hashString(JSON.stringify(item));
}

function matchesAll(item: unknown, match?: CustomTriggerMatch[]): boolean {
  if (!match || match.length === 0) return true;
  return match.every((m) => {
    const raw = getByPath(item, m.path);
    const val = raw == null ? "" : String(raw).toLowerCase();
    if (m.equals != null) return val === m.equals.toLowerCase();
    if (m.contains != null) return val.includes(m.contains.toLowerCase());
    return raw != null; // path present
  });
}

function renderTemplate(
  tpl: string | undefined,
  item: unknown,
  config: Record<string, unknown>
): string {
  if (!tpl) return "event";
  return tpl.replace(/\{\{([^}]+)\}\}/g, (_m, expr) => {
    const key = String(expr).trim();
    if (key.startsWith("config.")) {
      const v = config[key.slice("config.".length)];
      return v == null ? "" : String(v);
    }
    const v = getByPath(item, key);
    return v == null ? "" : String(v);
  });
}

// ---------------------------------------------------------------------------
// Composio action execution + delivery
// ---------------------------------------------------------------------------

let composioPromise: Promise<Composio | null> | null = null;
async function getComposio(): Promise<Composio | null> {
  if (composioPromise) return composioPromise;
  composioPromise = (async () => {
    const apiKey = env("COMPOSIO_API_KEY");
    if (!apiKey) return null;
    return new Composio({ apiKey });
  })();
  return composioPromise;
}

async function executeAction(
  tenantId: string,
  action: string,
  args: Record<string, unknown>,
  connectedAccountId?: string
): Promise<{ ok: boolean; data?: unknown; error?: string }> {
  // First-party integrations: ZOHOCLIQ_* slugs execute through our own Zoho
  // Cliq client (Cliq isn't a Composio toolkit) — same {ok,data|error} shape.
  if (action.toUpperCase().startsWith("ZOHOCLIQ_")) {
    const { executeZohoCliqAction } = await import("@/app/lib/zohoCliq");
    return executeZohoCliqAction(tenantId, action, args);
  }
  const composio = await getComposio();
  if (!composio) return { ok: false, error: "COMPOSIO_API_KEY not configured" };
  try {
    const resp = await (composio.tools as unknown as {
      execute: (
        slug: string,
        body: Record<string, unknown>
      ) => Promise<{ data?: unknown; successful?: boolean; error?: unknown }>;
    }).execute(action, {
      userId: tenantId,
      arguments: args,
      // Composio requires an explicit toolkit version for manual execution;
      // "latest" is rejected unless we opt out of the version pin. The agentic
      // tool path (createExecuteToolFn) sets the same flag — without it every
      // poll fails with "Toolkit version not specified".
      dangerouslySkipVersionCheck: true,
      ...(connectedAccountId ? { connectedAccountId } : {}),
    });
    if (resp && resp.successful === false) {
      return { ok: false, error: String(resp.error ?? "action failed"), data: resp.data };
    }
    return { ok: true, data: resp?.data ?? resp };
  } catch (err: any) {
    return { ok: false, error: err?.message ?? String(err) };
  }
}

async function deliverToTenant(
  tenantId: string,
  text: string,
  meta: Record<string, unknown>
): Promise<boolean> {
  // Team tenants deliver to the bound group chat; per-user tenants use their
  // own session (channel:senderId) or the channel's last session.
  let sessionId: string | undefined;
  let outChannel: Channel | undefined;
  const teamSess = await teamGroupSession(tenantId);
  if (teamSess) {
    sessionId = teamSess.sessionId;
    outChannel = teamSess.channel;
  } else {
    const colon = tenantId.indexOf(":");
    const channel = (colon > 0 ? tenantId.slice(0, colon) : "telegram") as Channel;
    const senderId = colon > 0 ? tenantId.slice(colon + 1) : tenantId;

    const candidate = await getSessionMeta(`${channel}:${senderId}`);
    sessionId = candidate?.sessionId;
    outChannel = candidate?.channel;
    if (!sessionId) {
      const last = await getLastSession(channel);
      sessionId = last?.sessionId;
      outChannel = last?.channel;
    }
  }
  if (!sessionId || !outChannel) return false;

  await sendOutboundRuntime({ channel: outChannel, sessionId, text: `🔔 ${text}` });
  await recordActivity(tenantId, {
    kind: "trigger",
    summary: text.slice(0, 140),
    meta,
  });
  return true;
}

// ---------------------------------------------------------------------------
// Poller
// ---------------------------------------------------------------------------

// Poll one subscription: execute its action, diff against the seen-set, deliver
// new matching events. Returns the number of events delivered.
async function pollOne(sub: CustomTriggerSubscription): Promise<number> {
  const store = getStore();
  const type = await getCustomTriggerType(sub.slug);
  if (!type) return 0;

  const sinceMs = sub.lastPolledAt ?? Date.now();
  const sinceVal =
    type.poll.sinceFormat === "ms" ? sinceMs : new Date(sinceMs).toISOString();
  const args: Record<string, unknown> = {
    ...(type.poll.baseArgs ?? {}),
    ...sub.config,
    ...(type.poll.sinceArg ? { [type.poll.sinceArg]: sinceVal } : {}),
  };

  const res = await executeAction(
    sub.tenantId,
    type.poll.action,
    args,
    sub.connectedAccountId
  );

  // Always advance the poll clock so a transient failure doesn't replay a huge
  // backlog on the next tick.
  sub.lastPolledAt = Date.now();
  await store.set(K_SUB(sub.subId), sub);

  if (!res.ok) {
    console.warn(
      `[customTriggers] poll ${sub.slug} (${sub.subId}) action ${type.poll.action} failed: ${res.error}`
    );
    // Don't fail silently forever, but DON'T assume every error is an expired
    // connection — a misconfigured action (wrong slug, missing required arg)
    // errors too, and telling the user to "reconnect" then is misleading noise.
    // Check the actual connection status and tailor the message. Dedupe per
    // tenant+toolkit (not per sub) so 6 monday subs don't send 6 messages.
    const warnKey = `ctrig:warned:${sub.tenantId}:${type.toolkit}`;
    const fresh = await store.set(warnKey, 1, { exSeconds: 6 * 3600, nx: true });
    if (fresh) {
      const conn = await toolkitConnectedAny(sub.tenantId, type.toolkit);
      const text = !conn.connected
        ? `Your ${type.toolkit} alerts can't run right now — the connection looks ` +
          `expired or revoked${conn.status ? ` (status: ${conn.status})` : ""}, so ` +
          `polls return nothing. Reconnect ${type.toolkit} and I'll resume watching ` +
          `your subscribed boards.`
        : `Heads up: your "${sub.slug}" alert keeps erroring even though ${type.toolkit} ` +
          `is connected — this looks like a misconfigured trigger, not an auth problem, ` +
          `so reconnecting won't help. Error: ${res.error}. Your other ${type.toolkit} ` +
          `alerts are unaffected.`;
      await deliverToTenant(sub.tenantId, text, {
        customTrigger: sub.slug,
        subId: sub.subId,
        pollError: true,
      });
    }
    return 0;
  }
  // Recovered — clear the warn flag so a future failure notifies again.
  await store.del(`ctrig:warned:${sub.tenantId}:${type.toolkit}`);

  const items = extractItems(res.data, type.poll.itemsPaths);
  if (!items.length) return 0;

  const seenArr = (await store.get<string[]>(K_SEEN(sub.subId))) ?? [];
  const seen = new Set(seenArr);

  let delivered = 0;
  const newlySeen: string[] = [];
  for (const item of items) {
    const id = itemId(item, type.poll.idPath);
    if (seen.has(id)) continue;
    newlySeen.push(id);
    if (!matchesAll(item, type.poll.match)) continue;
    // Fan out to automations whose composio trigger type names this custom
    // slug (workforces, jobs, …) — same contract as a real Composio webhook
    // (see composioWebhook.ts). Best-effort and independent of the chat
    // notification below. Dynamic import avoids a module cycle.
    try {
      const {
        matchComposio,
        eventMatchesFilter,
        isSelfGeneratedComposioEvent,
        isLowPriorityGmailEvent,
        fireAutomation,
      } = await import("@/app/lib/automations");
      if (
        !isSelfGeneratedComposioEvent(sub.slug, item) &&
        !isLowPriorityGmailEvent(sub.slug, item)
      ) {
        const rules = await matchComposio(sub.slug, sub.tenantId);
        for (const rule of rules) {
          if (
            rule.trigger.kind === "composio" &&
            eventMatchesFilter(item, rule.trigger.filter)
          ) {
            await fireAutomation(rule.id, "composio", deepCoerce(item));
          }
        }
      }
    } catch (err) {
      console.warn(`[customTriggers] automation fan-out failed: ${String(err)}`);
    }
    const title = renderTemplate(type.poll.titleTemplate, item, sub.config);
    // By default, explain what actually changed (AI), falling back to the
    // static title when the model is unavailable. Lead with the title for
    // at-a-glance context, then the human explanation.
    const explanation = await summarizeChangeAI(type, item, sub.config);
    const text = explanation ? `${title}\n${explanation}` : title;
    const ok = await deliverToTenant(sub.tenantId, text, {
      customTrigger: sub.slug,
      subId: sub.subId,
      toolkit: type.toolkit,
    });
    if (ok) delivered++;
  }

  if (newlySeen.length) {
    const merged = [...newlySeen, ...seenArr].slice(0, SEEN_CAP);
    await store.set(K_SEEN(sub.subId), merged);
  }
  return delivered;
}

// ---------------------------------------------------------------------------
// Debug / introspection (no-bearer op=debug_ctrig). Lets us see why a custom
// subscription isn't firing without shipping a guess.
// ---------------------------------------------------------------------------

export async function listAllCustomSubscriptions(): Promise<
  CustomTriggerSubscription[]
> {
  const store = getStore();
  const ids = await store.smembers(K_SUBS_INDEX);
  const out: CustomTriggerSubscription[] = [];
  for (const id of ids) {
    const sub = await store.get<CustomTriggerSubscription>(K_SUB(id));
    if (sub) out.push(sub);
  }
  return out;
}

// Run the same fetch+extract+match pipeline as pollOne but DON'T deliver and
// DON'T advance the clock — return a rich diagnostic so we can see exactly what
// the Composio action returned and why nothing fired.
export async function debugPollSub(
  subId: string
): Promise<Record<string, unknown>> {
  const store = getStore();
  const sub = await store.get<CustomTriggerSubscription>(K_SUB(subId));
  if (!sub) return { ok: false, error: `no sub ${subId}` };
  const type = await getCustomTriggerType(sub.slug);
  if (!type) return { ok: false, error: `no type ${sub.slug}` };

  const sinceMs = sub.lastPolledAt ?? Date.now();
  const sinceVal =
    type.poll.sinceFormat === "ms" ? sinceMs : new Date(sinceMs).toISOString();
  const args: Record<string, unknown> = {
    ...(type.poll.baseArgs ?? {}),
    ...sub.config,
    ...(type.poll.sinceArg ? { [type.poll.sinceArg]: sinceVal } : {}),
  };

  const res = await executeAction(
    sub.tenantId,
    type.poll.action,
    args,
    sub.connectedAccountId
  );

  const items = res.ok ? extractItems(res.data, type.poll.itemsPaths) : [];
  const seenArr = (await store.get<string[]>(K_SEEN(subId))) ?? [];
  const seen = new Set(seenArr);
  const sample = items.slice(0, 5).map((item) => ({
    id: itemId(item, type.poll.idPath),
    seen: seen.has(itemId(item, type.poll.idPath)),
    matches: matchesAll(item, type.poll.match),
    keys: item && typeof item === "object" ? Object.keys(item as object) : [],
    raw: item,
  }));

  let rawData: unknown = res.data;
  try {
    const s = JSON.stringify(res.data);
    if (s && s.length > 4000) rawData = s.slice(0, 4000) + "…(truncated)";
  } catch {
    /* ignore */
  }

  return {
    ok: true,
    sub,
    type: {
      slug: type.slug,
      toolkit: type.toolkit,
      action: type.poll.action,
      sinceArg: type.poll.sinceArg,
      itemsPaths: type.poll.itemsPaths,
      idPath: type.poll.idPath,
      match: type.poll.match,
    },
    argsSent: args,
    actionOk: res.ok,
    actionError: res.error ?? null,
    itemsFound: items.length,
    seenCount: seenArr.length,
    sample,
    rawData,
  };
}

// Run an arbitrary Composio action with arbitrary args (debug only).
export async function debugRunAction(
  tenantId: string,
  actionSlug: string,
  args: Record<string, unknown>,
  connectedAccountId?: string
): Promise<Record<string, unknown>> {
  const res = await executeAction(tenantId, actionSlug, args, connectedAccountId);
  let data: unknown = res.data;
  try {
    const s = JSON.stringify(res.data);
    if (s && s.length > 4000) data = s.slice(0, 4000) + "…(truncated)";
  } catch {
    /* ignore */
  }
  return { ok: res.ok, error: res.error ?? null, data };
}

// Fetch a Composio action's schema via the COMPOSIO_GET_TOOL_SCHEMAS action so
// we can confirm the correct argument names (e.g. board_id vs board_ids).
export async function debugActionSchema(
  tenantId: string,
  actionSlug: string
): Promise<Record<string, unknown>> {
  const res = await executeAction(tenantId, "COMPOSIO_GET_TOOL_SCHEMAS", {
    tool_slugs: [actionSlug],
  });
  return { ok: res.ok, error: res.error ?? null, data: res.data };
}

// Poll all subscriptions whose interval has elapsed. Bounded per tick so one
// cron run can't fan out unboundedly. Called from the daemon each minute.
export async function pollDueCustomSubscriptions(
  opts?: { maxSubs?: number }
): Promise<{ polled: number; delivered: number }> {
  const store = getStore();
  const ids = await store.smembers(K_SUBS_INDEX);
  const max = opts?.maxSubs ?? 25;
  const now = Date.now();

  let polled = 0;
  let delivered = 0;
  for (const id of ids) {
    if (polled >= max) break;
    const sub = await store.get<CustomTriggerSubscription>(K_SUB(id));
    if (!sub) {
      await store.srem(K_SUBS_INDEX, id); // drop dangling index entry
      continue;
    }
    const type = await getCustomTriggerType(sub.slug);
    const intervalMs = (type?.minIntervalSec ?? 120) * 1000;
    if (sub.lastPolledAt && now - sub.lastPolledAt < intervalMs) continue;
    polled++;
    try {
      delivered += await pollOne(sub);
    } catch (err: any) {
      console.warn(
        `[customTriggers] pollOne ${id} threw: ${err?.message ?? String(err)}`
      );
    }
  }
  return { polled, delivered };
}
