// app/lib/composioWebhook.ts
//
// Receive an event webhook from Composio, find the tenant it belongs to,
// format the JSON event into a natural-language summary, and send it to the
// tenant's Telegram chat.
//
// Composio posts events in one of three payload formats (V1/V2/V3). The
// fields live in different places per version, so we version-detect and
// normalize before doing anything else. Getting this wrong was the bug that
// made every event drop with "no tenant mapping".
//
//   V1: { trigger_name, connection_id, trigger_id, payload, log_id }
//   V2: { type, timestamp, log_id, data: { trigger_id, user_id, ...event } }
//   V3: { id, timestamp, type, metadata: { trigger_slug, trigger_id,
//         user_id, connected_account_id, auth_config_id }, data: {...event} }
//
// `user_id` (V2/V3) is the entity id we passed to triggers.create() on
// subscribe — i.e. our tenantId ("telegram:<chatId>"). So tenant resolution
// is: trigger_id → Redis map (set on subscribe), else user_id directly.
//
// Signature verification: Composio signs with Svix-style headers
// (`webhook-id`, `webhook-timestamp`, `webhook-signature: v1,<base64>`).
// When COMPOSIO_WEBHOOK_SECRET is set we verify via the SDK; otherwise we
// accept (dev mode). The previous implementation checked a non-existent
// `x-composio-signature` HMAC and would have rejected every signed webhook.

import { generateText } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";
import { Composio } from "@composio/core";

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import type { Channel } from "@/app/lib/identity";
import { tenantForTriggerInstance } from "@/app/lib/composioTriggers";
import {
  summarizeEvent,
  isSelfGeneratedComposioEvent,
  isLowPriorityGmailEvent,
} from "@/app/lib/automations";
import { getSessionMeta, getLastSession } from "@/app/lib/sessionMeta";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { recordActivity } from "@/app/lib/activityLog";

// Small ring buffer of recent webhook hits for observability — lets us (and
// the user) see whether Composio is actually POSTing events and whether they
// were delivered, without scraping function logs.
const WEBHOOK_LOG_KEY = "composio:webhook:log";
const WEBHOOK_LOG_CAP = 25;
type WebhookLogEntry = {
  ts: number;
  slug?: string;
  triggerId?: string;
  tenantId?: string;
  ok: boolean;
  error?: string;
};
async function recordWebhookHit(entry: WebhookLogEntry): Promise<void> {
  try {
    const store = getStore();
    await store.lpush(WEBHOOK_LOG_KEY, JSON.stringify(entry));
    await store.ltrim(WEBHOOK_LOG_KEY, 0, WEBHOOK_LOG_CAP - 1);
  } catch {
    // observability must never break delivery
  }
}
export async function getRecentWebhookHits(limit = 25): Promise<WebhookLogEntry[]> {
  try {
    const store = getStore();
    const raw = await store.lrange(WEBHOOK_LOG_KEY, 0, Math.max(0, limit - 1));
    const out: WebhookLogEntry[] = [];
    for (const r of raw) {
      try {
        out.push(JSON.parse(r) as WebhookLogEntry);
      } catch {
        // skip
      }
    }
    return out;
  } catch {
    return [];
  }
}

// Normalized event extracted from any payload version.
type NormalizedEvent = {
  triggerId: string; // best-effort: nano id or uuid
  altTriggerId?: string; // the other id form, for Redis lookup fallback
  triggerSlug: string;
  toolkitSlug?: string;
  userId?: string; // == our tenantId for V2/V3
  connectedAccountId?: string;
  payload: unknown; // the actual event data
};

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

function asRecord(v: unknown): Record<string, unknown> | null {
  return v && typeof v === "object" ? (v as Record<string, unknown>) : null;
}

// Derive a stable identifier for this event so retries and overlapping
// subscriptions that re-deliver the SAME event collapse to one. Prefer the
// content id baked into the payload (e.g. Gmail message_id) — that stays
// constant across every duplicate delivery of the same email — then fall back
// to the top-level event/log id and finally the Svix webhook-id header.
function stableEventId(
  evt: NormalizedEvent,
  parsed: unknown,
  headers: Record<string, string | null>
): string {
  const p = asRecord(evt.payload) ?? {};
  const content =
    p.message_id ??
    p.messageId ??
    p.id ??
    p.thread_id ??
    p.threadId;
  if (content != null && String(content)) return `c:${String(content)}`;
  const top = asRecord(parsed);
  const topId = top?.id ?? top?.log_id;
  if (topId != null && String(topId)) return `t:${String(topId)}`;
  const whId = headers["webhook-id"];
  if (whId) return `w:${whId}`;
  // Last resort: hash the payload itself. Without this, events from toolkits
  // that carry no id field skip dedupe entirely — Composio's retries then fire
  // the same automation N times for one event (duplicate rows, double emails).
  // Identical payload within the dedupe window ⇒ same event.
  try {
    const s = JSON.stringify(evt.payload ?? parsed ?? "");
    if (s && s !== "null" && s !== '""' && s !== "{}") {
      let h = 5381;
      for (let i = 0; i < s.length; i++) h = ((h * 33) ^ s.charCodeAt(i)) >>> 0;
      return `h:${h.toString(36)}:${s.length}`;
    }
  } catch {
    // unhashable payload — give up on dedupe for this event
  }
  return "";
}

// Detect version by shape and normalize. Mirrors the SDK's internal
// normalizeV1/V2/V3 so we don't depend on its private methods (and so we can
// parse without requiring a verified signature).
export function normalizeComposioPayload(raw: unknown): NormalizedEvent | null {
  const obj = asRecord(raw);
  if (!obj) return null;

  // V3: has metadata with trigger_slug + user_id.
  const meta = asRecord(obj.metadata);
  if (meta && (meta.trigger_slug || meta.trigger_id)) {
    const slug = String(meta.trigger_slug ?? meta.trigger_id ?? "");
    return {
      triggerId: String(meta.trigger_id ?? ""),
      triggerSlug: slug,
      toolkitSlug: slug.split("_")[0]?.toUpperCase(),
      userId: meta.user_id ? String(meta.user_id) : undefined,
      connectedAccountId: meta.connected_account_id
        ? String(meta.connected_account_id)
        : undefined,
      payload: obj.data ?? {},
    };
  }

  // V2: type + data.{trigger_id,user_id,...}
  const data = asRecord(obj.data);
  if (typeof obj.type === "string" && data && (data.trigger_id || data.user_id)) {
    const slug = String(obj.type).toUpperCase();
    const {
      trigger_id,
      trigger_nano_id,
      connection_id,
      connection_nano_id,
      user_id,
      ...rest
    } = data as Record<string, unknown>;
    return {
      triggerId: String(trigger_nano_id ?? trigger_id ?? ""),
      altTriggerId: trigger_id ? String(trigger_id) : undefined,
      triggerSlug: slug,
      toolkitSlug: slug.split("_")[0]?.toUpperCase(),
      userId: user_id ? String(user_id) : undefined,
      connectedAccountId: connection_nano_id
        ? String(connection_nano_id)
        : connection_id
          ? String(connection_id)
          : undefined,
      payload: rest,
    };
  }

  // V1: flat trigger_name + trigger_id + payload (no user_id).
  if (typeof obj.trigger_name === "string" && obj.trigger_id) {
    const slug = String(obj.trigger_name);
    return {
      triggerId: String(obj.trigger_id),
      triggerSlug: slug,
      toolkitSlug: slug.split("_")[0]?.toUpperCase(),
      userId: undefined, // V1 has no user_id — must resolve via Redis map
      connectedAccountId: obj.connection_id ? String(obj.connection_id) : undefined,
      payload: obj.payload ?? {},
    };
  }

  return null;
}

// Verify the Svix-style signature via the SDK when a secret is configured.
// Returns true to proceed (verified OR no-secret dev mode), false to reject.
async function verifySignature(
  rawBody: string,
  headers: Record<string, string | null>
): Promise<boolean> {
  const secret = env("COMPOSIO_WEBHOOK_SECRET");
  if (!secret) return true; // dev mode: accept

  const id = headers["webhook-id"];
  const timestamp = headers["webhook-timestamp"];
  const signature = headers["webhook-signature"];
  if (!id || !timestamp || !signature) {
    console.warn("[composioWebhook] secret set but Svix headers missing");
    // Lenient by default (see below) so we don't drop real events over a
    // header-shape mismatch; strict mode rejects.
    return (env("COMPOSIO_WEBHOOK_STRICT") ?? "false") !== "true";
  }
  const composio = await getComposio();
  if (!composio) return false;
  try {
    await (composio as unknown as {
      triggers: { verifyWebhook: (p: Record<string, unknown>) => Promise<unknown> };
    }).triggers.verifyWebhook({
      id,
      payload: rawBody,
      secret,
      signature,
      timestamp,
    });
    return true;
  } catch (err: any) {
    console.warn(`[composioWebhook] signature verification failed: ${err?.message ?? String(err)}`);
    // Default lenient: a signature quirk should NOT silently swallow a real
    // email notification. Set COMPOSIO_WEBHOOK_STRICT=true to reject instead
    // (recommended once you've confirmed verification works in logs).
    return (env("COMPOSIO_WEBHOOK_STRICT") ?? "false") !== "true";
  }
}

function prettySlug(slug: string): string {
  return slug
    .replace(/_/g, " ")
    .toLowerCase()
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

// Deterministic notification text built straight from the event's salient
// fields — ZERO tokens. This is the default: inbound mail/events used to cost
// one gpt-4o-mini call EACH just to phrase a Telegram ping, which is what was
// burning money on every new email. We reuse the same field extractor the
// automation runtime uses (summarizeEvent) so the notify matches the payload.
function formatEventMessageDeterministic(
  triggerName: string,
  payload: unknown
): string {
  let fields: Record<string, string> = {};
  try {
    const { fields: json } = summarizeEvent(payload);
    if (json) fields = JSON.parse(json) as Record<string, string>;
  } catch {
    // fall through to slug-only
  }

  const from = fields.from ?? "";
  const subject = fields.subject ?? "";
  const snippet = (fields.snippet ?? "").replace(/\s+/g, " ").trim();

  if (triggerName.toUpperCase().startsWith("GMAIL_")) {
    const head = `📧 ${from || "New email"}${subject ? ` — ${subject}` : ""}`;
    return snippet ? `${head}\n${snippet.slice(0, 160)}` : head;
  }

  // Generic event: pretty trigger name + the couple of fields we did extract.
  const bits = [subject, from && `from ${from}`, snippet && snippet.slice(0, 120)]
    .filter(Boolean)
    .join(" · ");
  return bits ? `${prettySlug(triggerName)}: ${bits}` : prettySlug(triggerName);
}

// LLM phrasing is now strictly opt-in (WEBHOOK_FORMAT_LLM=true). Off by default
// so routine notifications spend nothing.
async function formatEventMessage(
  triggerName: string,
  payload: unknown
): Promise<string> {
  if ((env("WEBHOOK_FORMAT_LLM") ?? "false") !== "true") {
    return formatEventMessageDeterministic(triggerName, payload);
  }
  const modelName = env("WEBHOOK_FORMAT_MODEL") ?? "gpt-4o-mini";
  try {
    const result = await generateText({
      model: textAuxModel(modelName),
      temperature: 0.3,
      system: [
        "You write short, friendly Telegram notifications from raw webhook",
        "event payloads — like a heads-up from a helpful assistant. 1-2",
        "sentences, casual, concrete: what happened, who/what's involved.",
        "Include the most relevant link if there is one. No buzzwords.",
      ].join("\n"),
      prompt:
        `Event: ${triggerName}\n\nPayload:\n` +
        JSON.stringify(payload, null, 2).slice(0, 4000),
    });
    return result.text.trim();
  } catch {
    return formatEventMessageDeterministic(triggerName, payload);
  }
}

export type DispatchResult =
  | { ok: true; tenantId: string; deliveredTo: string; triggerSlug: string }
  | { ok: false; error: string };

// Accepts the raw request so we can verify signatures + version-detect.
export async function dispatchComposioWebhook(args: {
  rawBody: string;
  headers?: Record<string, string | null>;
}): Promise<DispatchResult> {
  const headers = args.headers ?? {};

  const okSig = await verifySignature(args.rawBody, headers);
  if (!okSig) return { ok: false, error: "signature verification failed" };

  let parsed: unknown;
  try {
    parsed = JSON.parse(args.rawBody);
  } catch {
    return { ok: false, error: "bad JSON" };
  }

  const evt = normalizeComposioPayload(parsed);
  if (!evt) {
    console.warn(
      `[composioWebhook] unrecognized payload shape: ${args.rawBody.slice(0, 300)}`
    );
    await recordWebhookHit({ ts: Date.now(), ok: false, error: "unrecognized payload shape" });
    return { ok: false, error: "unrecognized payload shape" };
  }

  console.log(
    `[composioWebhook] event slug=${evt.triggerSlug} triggerId=${evt.triggerId} userId=${evt.userId ?? "(none)"}`
  );

  // Resolve tenant: Redis map by trigger id (set on subscribe), trying both
  // id forms, then fall back to the userId Composio echoes (== our tenantId).
  let tenantId: string | null = null;
  if (evt.triggerId) tenantId = await tenantForTriggerInstance(evt.triggerId);
  if (!tenantId && evt.altTriggerId) {
    tenantId = await tenantForTriggerInstance(evt.altTriggerId);
  }
  if (!tenantId && evt.userId) tenantId = evt.userId;

  if (!tenantId) {
    await recordWebhookHit({
      ts: Date.now(),
      slug: evt.triggerSlug,
      triggerId: evt.triggerId,
      ok: false,
      error: "no tenant mapping",
    });
    return {
      ok: false,
      error: `no tenant mapping for trigger ${evt.triggerId || evt.triggerSlug}`,
    };
  }

  // Collapse duplicate deliveries. Composio retries slow/failed webhooks and a
  // tenant can accumulate overlapping trigger subscriptions, so the SAME event
  // arrives several times — which was firing the notify AND the automation N
  // times for one email (e.g. the same HubSpot code logged to A4/A5/A6). First
  // delivery wins for a short window; the rest short-circuit before we send or
  // fan out to automations.
  const eventId = stableEventId(evt, parsed, headers);
  if (eventId) {
    const dedupeKey = `composio:webhook:seen:${tenantId}:${evt.triggerSlug}:${eventId}`;
    const fresh = await getStore().set(dedupeKey, "1", {
      exSeconds: 600,
      nx: true,
    });
    if (!fresh) {
      console.log(
        `[composioWebhook] deduped duplicate event slug=${evt.triggerSlug} id=${eventId} tenant=${tenantId}`
      );
      await recordWebhookHit({
        ts: Date.now(),
        slug: evt.triggerSlug,
        triggerId: evt.triggerId,
        tenantId,
        ok: true,
        error: "duplicate (deduped)",
      });
      return {
        ok: true,
        tenantId,
        deliveredTo: "(deduped)",
        triggerSlug: evt.triggerSlug,
      };
    }
  }

  // Deterministic, token-free pre-gate — decided BEFORE we notify or fan out.
  //   self-mail (your own draft/sent): drop entirely, no ping, no agent turn.
  //   low-priority (promotions/social/spam): no ping + no agent turn, but still
  //     logged for observability. Set GMAIL_AUTOMATION_INCLUDE_BULK=1 to keep
  //     pinging on bulk; WEBHOOK_NOTIFY_BULK=1 to ping-but-not-automate on bulk.
  const selfMail = isSelfGeneratedComposioEvent(evt.triggerSlug, evt.payload);
  if (selfMail) {
    await recordWebhookHit({
      ts: Date.now(),
      slug: evt.triggerSlug,
      triggerId: evt.triggerId,
      tenantId,
      ok: true,
      error: "self-generated (skipped)",
    });
    return {
      ok: true,
      tenantId,
      deliveredTo: "(self, skipped)",
      triggerSlug: evt.triggerSlug,
    };
  }
  const lowPriority = isLowPriorityGmailEvent(evt.triggerSlug, evt.payload);
  const notifyBulk = (env("WEBHOOK_NOTIFY_BULK") ?? "0") === "1";

  // Find a session for this tenant. Team tenants deliver to the bound group
  // chat; per-user tenants use their session (channel:senderId) or last session.
  let sessionId: string | undefined;
  let outChannel: Channel | undefined;
  const { teamGroupSession } = await import("@/app/lib/teams");
  const teamSess = await teamGroupSession(tenantId);
  if (teamSess) {
    sessionId = teamSess.sessionId;
    outChannel = teamSess.channel;
  } else {
    const colon = tenantId.indexOf(":");
    const channel = colon > 0 ? tenantId.slice(0, colon) : "telegram";
    const senderId = colon > 0 ? tenantId.slice(colon + 1) : tenantId;

    const candidate = await getSessionMeta(`${channel}:${senderId}`);
    sessionId = candidate?.sessionId;
    outChannel = candidate?.channel;
    if (!sessionId) {
      const last = await getLastSession(channel as never);
      sessionId = last?.sessionId;
      outChannel = last?.channel;
    }
  }
  if (!sessionId || !outChannel) {
    await recordWebhookHit({
      ts: Date.now(),
      slug: evt.triggerSlug,
      triggerId: evt.triggerId,
      tenantId,
      ok: false,
      error: "no active session",
    });
    return {
      ok: false,
      error: `no active session to deliver to for tenant ${tenantId}`,
    };
  }

  // Skip the Telegram ping for low-priority bulk mail (unless explicitly kept).
  // formatEventMessage is deterministic by default now, so even when we DO ping
  // it costs zero tokens.
  const notified = !lowPriority || notifyBulk;
  if (notified) {
    const message = await formatEventMessage(evt.triggerSlug, evt.payload);
    await sendOutboundRuntime({
      channel: outChannel,
      sessionId,
      text: `🔔 ${message}`,
    });
  }

  console.log(
    `[composioWebhook] ${notified ? "delivered" : "logged (bulk, no ping)"} slug=${evt.triggerSlug} → tenant=${tenantId} session=${sessionId}`
  );
  await recordWebhookHit({
    ts: Date.now(),
    slug: evt.triggerSlug,
    triggerId: evt.triggerId,
    tenantId,
    ok: true,
  });
  // Mirror to the per-tenant activity log so the dashboard's Recent Activity
  // surfaces real trigger deliveries (not just chat-side commands).
  await recordActivity(tenantId, {
    kind: "trigger",
    summary: `trigger event: ${evt.triggerSlug}`,
    meta: {
      triggerId: evt.triggerId,
      triggerSlug: evt.triggerSlug,
      toolkit: evt.toolkitSlug,
      connectedAccountId: evt.connectedAccountId,
      sessionId,
    },
  });

  // Fire any user-defined automations whose Composio trigger type matches this
  // event (subject to their substring filter). Independent of the notify above
  // — a tenant can have a "notify me" subscription AND an automation on the
  // same event. Best-effort: automation failures don't fail the dispatch.
  if (lowPriority) {
    // Promotions / social / spam — already skipped the ping; never fire an
    // agent turn either. (selfMail returned far above.)
    return { ok: true, tenantId, deliveredTo: sessionId, triggerSlug: evt.triggerSlug };
  }
  try {
    const { matchComposio, eventMatchesFilter, fireAutomation } = await import(
      "@/app/lib/automations"
    );
    const matches = await matchComposio(evt.triggerSlug, tenantId);
    for (const rule of matches) {
      if (
        rule.trigger.kind === "composio" &&
        eventMatchesFilter(evt.payload, rule.trigger.filter)
      ) {
        await fireAutomation(rule.id, "composio", evt.payload);
      }
    }
  } catch (err) {
    console.warn(`[composioWebhook] automation fan-out failed: ${String(err)}`);
  }

  return { ok: true, tenantId, deliveredTo: sessionId, triggerSlug: evt.triggerSlug };
}
