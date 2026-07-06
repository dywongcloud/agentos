// app/lib/zohoCliq.ts
//
// First-party MULTITENANT Zoho Cliq integration. Cliq is not a Composio
// toolkit, so we run the OAuth + REST surface ourselves while keeping the
// exact same tenant model as everything else: connections are keyed by the
// tenant identity (telegram:<id>, or team:<id> for a shared workspace — a
// team connect makes Cliq shared for the whole team automatically).
//
// OAuth (Zoho OAuth2, multi-DC):
//   - zohoCliqAuthUrl(tenant) builds the accounts.zoho.com consent URL with a
//     nonce state persisted in Redis (CSRF-safe, 15 min TTL) that also carries
//     the chat to notify on completion.
//   - /api/zoho/oauth/callback (see app/api/zoho/oauth/callback/route.ts)
//     exchanges the code AT THE DC the callback names (`accounts-server`
//     param) — Zoho is multi-datacenter (com/eu/in/com.au/jp) and tokens only
//     exchange/refresh on their home DC. The Cliq API domain is derived from
//     the same DC (accounts.zoho.eu → cliq.zoho.eu).
//   - Tokens auto-refresh with a 60s skew; a 401 retries once post-refresh.
//
// Actions: executeZohoCliqAction(tenant, "ZOHOCLIQ_*", args) mirrors the
// Composio executor shape ({ok, data|error}) so the custom-trigger poller and
// agent tools can treat Zoho slugs exactly like Composio slugs. Covers the v3
// chats / messages / threads / scheduledmessages / pinmessages / mypins
// surfaces plus a generic API_REQUEST escape hatch.
//
// Operator setup (one-time): register a Zoho API console "Server-based" client
// with redirect URI https://<host>/api/zoho/oauth/callback and set
// ZOHO_CLIENT_ID + ZOHO_CLIENT_SECRET. Scopes override: ZOHO_CLIQ_SCOPES.

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";

// --- storage ----------------------------------------------------------------

export type ZohoCliqTokens = {
  accessToken: string;
  refreshToken?: string;
  expiresAt: number; // epoch ms
  accountsServer: string; // e.g. https://accounts.zoho.com
  apiBase: string; // e.g. https://cliq.zoho.com/api/v3
  connectedAt: number;
  // What Zoho's token response actually said it granted — can be narrower
  // than DEFAULT_SCOPES if the app's Zoho API Console registration hasn't
  // been configured to allow every requested scope (see handleZohoCallback).
  grantedScope?: string;
};

type PendingState = {
  tenantId: string;
  channel?: string;
  sessionId?: string;
  createdAt: number;
};

const tokensKey = (tenantId: string) => `zoho:cliq:tokens:${tenantId}`;
const stateKey = (nonce: string) => `zoho:cliq:state:${nonce}`;

// Zoho Cliq scopes only exist as <Resource>.<CREATE|READ|UPDATE|DELETE> — there
// is no ".ALL" suffix for Chats/Channels/Messages, and Threads/ScheduledMessages/
// PinMessages/MyPins are NOT their own scope categories (Zoho rejects the whole
// auth request if any single scope in the list is unrecognized). Those features
// are exposed through the Chats/Messages REST resources, so full CRUD on Chats +
// Messages (+ Channels, for the generic API_REQUEST escape hatch) covers them.
const DEFAULT_SCOPES =
  "ZohoCliq.Chats.CREATE,ZohoCliq.Chats.READ,ZohoCliq.Chats.UPDATE,ZohoCliq.Chats.DELETE,ZohoCliq.Channels.CREATE,ZohoCliq.Channels.READ,ZohoCliq.Channels.UPDATE,ZohoCliq.Channels.DELETE,ZohoCliq.Messages.CREATE,ZohoCliq.Messages.READ,ZohoCliq.Messages.UPDATE,ZohoCliq.Messages.DELETE";

function scopes(): string {
  return env("ZOHO_CLIQ_SCOPES") ?? DEFAULT_SCOPES;
}

export function zohoConfigured(): boolean {
  return !!env("ZOHO_CLIENT_ID") && !!env("ZOHO_CLIENT_SECRET");
}

function redirectUri(): string {
  const base =
    env("ZOHO_REDIRECT_BASE_URL") ??
    env("APP_BASE_URL") ??
    (process.env.VERCEL_PROJECT_PRODUCTION_URL
      ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`
      : "https://agentos-claw.vercel.app");
  return `${base.replace(/\/+$/, "")}/api/zoho/oauth/callback`;
}

// Cliq API base for a Zoho accounts DC: accounts.zoho.eu → https://cliq.zoho.eu.
function apiBaseFor(accountsServer: string): string {
  try {
    const host = new URL(accountsServer).host; // accounts.zoho.eu
    const suffix = host.replace(/^accounts\./, ""); // zoho.eu
    return `https://cliq.${suffix}/api/v3`;
  } catch {
    return "https://cliq.zoho.com/api/v3";
  }
}

// --- auth url + callback ------------------------------------------------------

export async function zohoCliqAuthUrl(args: {
  tenantId: string;
  channel?: string;
  sessionId?: string;
}): Promise<{ ok: true; url: string } | { ok: false; error: string }> {
  if (!zohoConfigured()) {
    return {
      ok: false,
      error:
        "Zoho is not configured — the operator must set ZOHO_CLIENT_ID and " +
        "ZOHO_CLIENT_SECRET (Zoho API console, redirect URI " +
        redirectUri() +
        ").",
    };
  }
  const nonce = globalThis.crypto.randomUUID().replace(/-/g, "");
  const pending: PendingState = {
    tenantId: args.tenantId,
    channel: args.channel,
    sessionId: args.sessionId,
    createdAt: Date.now(),
  };
  await getStore().set(stateKey(nonce), pending, { exSeconds: 15 * 60 });

  const u = new URL("https://accounts.zoho.com/oauth/v2/auth");
  u.searchParams.set("response_type", "code");
  u.searchParams.set("client_id", env("ZOHO_CLIENT_ID") ?? "");
  u.searchParams.set("scope", scopes());
  u.searchParams.set("redirect_uri", redirectUri());
  u.searchParams.set("access_type", "offline"); // get a refresh_token
  u.searchParams.set("prompt", "consent"); // re-consent re-issues refresh_token
  u.searchParams.set("state", nonce);
  return { ok: true, url: u.toString() };
}

export async function handleZohoCallback(params: {
  code: string;
  state: string;
  accountsServer?: string | null;
}): Promise<
  | { ok: true; tenantId: string; channel?: string; sessionId?: string }
  | { ok: false; error: string }
> {
  const store = getStore();
  const pending = await store.get<PendingState>(stateKey(params.state));
  if (!pending) return { ok: false, error: "invalid or expired state — restart the connect flow" };
  await store.del(stateKey(params.state)); // single-use

  const accountsServer = params.accountsServer || "https://accounts.zoho.com";
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code: params.code,
    client_id: env("ZOHO_CLIENT_ID") ?? "",
    client_secret: env("ZOHO_CLIENT_SECRET") ?? "",
    redirect_uri: redirectUri(),
  });
  const res = await fetch(`${accountsServer.replace(/\/+$/, "")}/oauth/v2/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: body.toString(),
  });
  const json = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok || !json.access_token) {
    return {
      ok: false,
      error: `token exchange failed: ${String(json.error ?? res.status)}`,
    };
  }

  // Zoho's token response's `scope` field is what was ACTUALLY granted — this
  // can be narrower than what the authorization URL requested if the app's
  // registration on Zoho's own API Console (api-console.zoho.com) hasn't been
  // configured to allow every requested scope; Zoho silently drops
  // unconfigured scopes rather than erroring at the authorize step, and the
  // gap only surfaces later as a per-call "oauthtoken_scope_invalid" 401.
  // Logged so a scope mismatch is diagnosable from production logs instead of
  // reverse-engineered from a user's bug report after the fact.
  const grantedScope = typeof json.scope === "string" ? json.scope : undefined;
  console.error(
    `[zohoCliq] token exchange granted scope="${grantedScope ?? "(not returned)"}" ` +
      `requested="${scopes()}"`
  );

  const tokens: ZohoCliqTokens = {
    accessToken: String(json.access_token),
    refreshToken: json.refresh_token ? String(json.refresh_token) : undefined,
    expiresAt: Date.now() + (Number(json.expires_in ?? 3600) - 60) * 1000,
    accountsServer,
    apiBase: apiBaseFor(accountsServer),
    connectedAt: Date.now(),
    grantedScope,
  };
  // Preserve an earlier refresh_token if Zoho omitted one on re-consent. NOTE:
  // a preserved refresh_token can be bound to an OLDER, narrower scope grant
  // than this exchange's fresh access_token — if so, the access_token works
  // until it expires, then refreshTokens() silently reverts to the old scope
  // (see the SCOPE CHANGED ON REFRESH log in refreshTokens). Logged here so
  // the fallback firing at all is visible, since Zoho's own docs say a fresh
  // reconsent should normally generate a new refresh_token each time.
  if (!tokens.refreshToken) {
    console.error(`[zohoCliq] token exchange omitted refresh_token — falling back to prior one`);
    const prior = await store.get<ZohoCliqTokens>(tokensKey(pending.tenantId));
    if (prior?.refreshToken) tokens.refreshToken = prior.refreshToken;
  }
  await store.set(tokensKey(pending.tenantId), tokens);
  return {
    ok: true,
    tenantId: pending.tenantId,
    channel: pending.channel,
    sessionId: pending.sessionId,
  };
}

// --- token access + refresh ----------------------------------------------------

export async function zohoCliqConnected(
  tenantId: string
): Promise<{ connected: boolean; apiBase?: string; connectedAt?: number; grantedScope?: string }> {
  const t = await getStore().get<ZohoCliqTokens>(tokensKey(tenantId));
  if (!t) return { connected: false };
  return { connected: true, apiBase: t.apiBase, connectedAt: t.connectedAt, grantedScope: t.grantedScope };
}

export async function disconnectZohoCliq(tenantId: string): Promise<void> {
  await getStore().del(tokensKey(tenantId));
}

async function refreshTokens(tenantId: string, t: ZohoCliqTokens): Promise<ZohoCliqTokens | null> {
  if (!t.refreshToken) return null;
  const body = new URLSearchParams({
    grant_type: "refresh_token",
    refresh_token: t.refreshToken,
    client_id: env("ZOHO_CLIENT_ID") ?? "",
    client_secret: env("ZOHO_CLIENT_SECRET") ?? "",
  });
  const res = await fetch(`${t.accountsServer.replace(/\/+$/, "")}/oauth/v2/token`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: body.toString(),
  });
  const json = (await res.json().catch(() => ({}))) as Record<string, unknown>;
  if (!res.ok || !json.access_token) return null;
  // A refresh_token grant is scoped to whatever the refresh_token ITSELF was
  // originally issued with — if that refresh_token predates a scope widening
  // (e.g. this tenant's stored refresh_token survived across several earlier
  // reconnects via the "preserve an earlier refresh_token" fallback in
  // handleZohoCallback), the refreshed access_token can silently carry an
  // OLDER, NARROWER scope than the access_token this same refresh call is
  // replacing — invisible until the next API call 401s on a missing scope.
  // Logged so a scope regression on refresh is directly provable, not guessed.
  const refreshedScope = typeof json.scope === "string" ? json.scope : undefined;
  if (refreshedScope !== t.grantedScope) {
    console.error(
      `[zohoCliq] SCOPE CHANGED ON REFRESH: was="${t.grantedScope ?? "(unknown)"}" ` +
        `now="${refreshedScope ?? "(not returned)"}"`
    );
  }
  const next: ZohoCliqTokens = {
    ...t,
    accessToken: String(json.access_token),
    expiresAt: Date.now() + (Number(json.expires_in ?? 3600) - 60) * 1000,
    ...(refreshedScope ? { grantedScope: refreshedScope } : {}),
  };
  await getStore().set(tokensKey(tenantId), next);
  return next;
}

async function freshTokens(tenantId: string): Promise<ZohoCliqTokens | null> {
  let t = await getStore().get<ZohoCliqTokens>(tokensKey(tenantId));
  if (!t) return null;
  if (Date.now() >= t.expiresAt) t = (await refreshTokens(tenantId, t)) ?? t;
  return t;
}

// Authenticated Cliq API request with auto-refresh and one 401 retry.
export async function zohoCliqRequest(
  tenantId: string,
  method: string,
  path: string,
  opts?: { body?: unknown; query?: Record<string, string | number | undefined> }
): Promise<{ ok: boolean; status: number; data?: unknown; error?: string }> {
  let t = await freshTokens(tenantId);
  if (!t) {
    return { ok: false, status: 401, error: "Zoho Cliq is not connected for this account" };
  }
  const doFetch = async (tok: ZohoCliqTokens) => {
    const u = new URL(`${tok.apiBase}${path.startsWith("/") ? path : `/${path}`}`);
    for (const [k, v] of Object.entries(opts?.query ?? {})) {
      if (v !== undefined && v !== null && String(v) !== "") u.searchParams.set(k, String(v));
    }
    return fetch(u.toString(), {
      method: method.toUpperCase(),
      headers: {
        Authorization: `Zoho-oauthtoken ${tok.accessToken}`,
        ...(opts?.body !== undefined ? { "content-type": "application/json" } : {}),
      },
      ...(opts?.body !== undefined ? { body: JSON.stringify(opts.body) } : {}),
    });
  };

  let res = await doFetch(t);
  if (res.status === 401) {
    const refreshed = await refreshTokens(tenantId, t);
    if (refreshed) {
      t = refreshed;
      res = await doFetch(t);
    }
  }
  const text = await res.text();
  let data: unknown = text;
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    // non-JSON body — return raw text
  }
  // Zoho's v3 API wraps every response in an envelope: {url, type, data:
  // {...actual payload...}} (confirmed against Zoho's own docs for the chat
  // creation endpoint, and consistent with v3's documented uniform response
  // shape). Unwrapping here means every ZOHOCLIQ_* action returns the real
  // payload directly (e.g. chat_id at the top level), not nested one level
  // deeper than callers would reasonably guess — the previous double-nesting
  // (result.data.data.chat_id) is exactly the kind of thing an LLM caller
  // misreads, e.g. grabbing creator.id (a person's id) instead of chat_id
  // and then using it as a chat_id in a follow-up call, which Zoho rejects
  // with a misleading "does not have the required scope" 401 rather than a
  // clean 404 for the nonexistent resource.
  const unwrapped =
    data &&
    typeof data === "object" &&
    "data" in (data as Record<string, unknown>) &&
    ("type" in (data as Record<string, unknown>) || "url" in (data as Record<string, unknown>))
      ? (data as { data: unknown }).data
      : data;
  if (!res.ok) {
    const msg =
      (data as { message?: string } | null)?.message ??
      `Zoho Cliq API ${res.status}: ${text.slice(0, 200)}`;
    console.error(
      `[zohoCliq] ${method.toUpperCase()} ${path} -> ${res.status} error=${msg} grantedScope="${t.grantedScope ?? "(unknown)"}"`
    );
    return { ok: false, status: res.status, data, error: msg };
  }
  return { ok: true, status: res.status, data: unwrapped };
}

// --- action registry -------------------------------------------------------------

// Composio-shaped result so callers (agent tool, custom-trigger poller) can
// treat ZOHOCLIQ_* slugs identically to Composio slugs. `status` is the raw
// Zoho HTTP status when known — callers MUST use this (not the error string)
// to decide whether a failure means "reconnect" (401) vs. "real action
// problem, e.g. bad chat_id" (400/404/etc) — see zohoCliqTools.ts.
export type ZohoExecResult = { ok: boolean; data?: unknown; error?: string; status?: number };

export const ZOHO_CLIQ_ACTIONS: Record<string, string> = {
  ZOHOCLIQ_LIST_CHATS: "List the user's chats. Args: {limit?}",
  ZOHOCLIQ_GET_CHAT: "Get one chat. Args: {chat_id}",
  ZOHOCLIQ_CREATE_CHAT:
    "Create (or reuse) a 1:1 or group chat. Args: {user_id} for a direct chat with one Cliq user_id " +
    "(NOT a name/email — if you don't already know their numeric Cliq user_id from a prior ZOHOCLIQ_LIST_CHATS " +
    "result, ask the user for it, there is no name/email lookup action), or {user_ids: string[], title} " +
    "(2-50 ids) for a group chat. Returns the new/existing chat's chat_id to use with ZOHOCLIQ_SEND_MESSAGE.",
  ZOHOCLIQ_LIST_MESSAGES:
    "List messages in a chat. Args: {chat_id, limit?, fromtime? (epoch ms), totime? (epoch ms)}",
  ZOHOCLIQ_SEND_MESSAGE: "Send a message. Args: {chat_id, text}",
  ZOHOCLIQ_SEND_THREAD_MESSAGE:
    "Reply into a thread. Args: {thread_chat_id, text}",
  ZOHOCLIQ_CREATE_THREAD:
    "Start a thread on a message. Args: {chat_id, message_id, text, title?}",
  ZOHOCLIQ_LIST_SCHEDULED_MESSAGES: "List scheduled messages. Args: {}",
  ZOHOCLIQ_SCHEDULE_MESSAGE:
    "Schedule a message. Args: {chat_id, text, scheduled_time (epoch ms or ISO), timezone?}",
  ZOHOCLIQ_DELETE_SCHEDULED_MESSAGE: "Delete a scheduled message. Args: {message_id}",
  ZOHOCLIQ_PIN_MESSAGE:
    "Pin a message in a chat. Args: {chat_id, message_id, expiry_time? (epoch ms)}",
  ZOHOCLIQ_LIST_PINNED_MESSAGES: "List a chat's pinned messages. Args: {chat_id}",
  ZOHOCLIQ_LIST_MY_PINS: "List the user's My Pins. Args: {}",
  ZOHOCLIQ_API_REQUEST:
    "Generic escape hatch for any Cliq v3 endpoint. Args: {method, path, body? (object), query? (object)} — path is relative to /api/v3, e.g. '/channels'.",
};

function str(v: unknown): string {
  return v == null ? "" : String(v);
}

export async function executeZohoCliqAction(
  tenantId: string,
  action: string,
  args: Record<string, unknown>
): Promise<ZohoExecResult> {
  const a = action.toUpperCase().trim();
  const chatId = str(args.chat_id ?? args.chatId);
  try {
    switch (a) {
      case "ZOHOCLIQ_LIST_CHATS":
        return norm(await zohoCliqRequest(tenantId, "GET", "/chats", {
          query: { limit: args.limit as number | undefined },
        }));
      case "ZOHOCLIQ_GET_CHAT":
        return norm(await zohoCliqRequest(tenantId, "GET", `/chats/${chatId}`));
      case "ZOHOCLIQ_CREATE_CHAT": {
        const userIds = Array.isArray(args.user_ids)
          ? (args.user_ids as unknown[]).map(str).filter(Boolean)
          : [];
        const body =
          userIds.length > 0
            ? { chat_type: "group_chat", user_ids: userIds, title: str(args.title) }
            : { chat_type: "direct_message", user_id: str(args.user_id ?? args.userId) };
        return norm(await zohoCliqRequest(tenantId, "POST", "/chats", { body }));
      }
      case "ZOHOCLIQ_LIST_MESSAGES":
        return norm(
          await zohoCliqRequest(tenantId, "GET", `/chats/${chatId}/messages`, {
            query: {
              limit: args.limit as number | undefined,
              fromtime: args.fromtime as number | undefined,
              totime: args.totime as number | undefined,
            },
          })
        );
      case "ZOHOCLIQ_SEND_MESSAGE":
        return norm(
          await zohoCliqRequest(tenantId, "POST", `/chats/${chatId}/messages`, {
            body: { text: str(args.text) },
          })
        );
      case "ZOHOCLIQ_SEND_THREAD_MESSAGE":
        return norm(
          await zohoCliqRequest(
            tenantId,
            "POST",
            `/chats/${str(args.thread_chat_id ?? args.threadChatId)}/messages`,
            { body: { text: str(args.text) } }
          )
        );
      case "ZOHOCLIQ_CREATE_THREAD":
        return norm(
          await zohoCliqRequest(tenantId, "POST", `/chats/${chatId}/threads`, {
            body: {
              message_id: str(args.message_id ?? args.messageId),
              text: str(args.text),
              ...(args.title ? { title: str(args.title) } : {}),
            },
          })
        );
      case "ZOHOCLIQ_LIST_SCHEDULED_MESSAGES":
        return norm(await zohoCliqRequest(tenantId, "GET", "/scheduledmessages"));
      case "ZOHOCLIQ_SCHEDULE_MESSAGE": {
        const rawTime = args.scheduled_time ?? args.scheduledTime;
        const ms =
          typeof rawTime === "number"
            ? rawTime
            : Date.parse(str(rawTime)) || Number(str(rawTime));
        return norm(
          await zohoCliqRequest(tenantId, "POST", "/scheduledmessages", {
            body: {
              chat_id: chatId,
              text: str(args.text),
              scheduled_time: ms,
              ...(args.timezone ? { timezone: str(args.timezone) } : {}),
            },
          })
        );
      }
      case "ZOHOCLIQ_DELETE_SCHEDULED_MESSAGE":
        return norm(
          await zohoCliqRequest(
            tenantId,
            "DELETE",
            `/scheduledmessages/${str(args.message_id ?? args.messageId)}`
          )
        );
      case "ZOHOCLIQ_PIN_MESSAGE":
        return norm(
          await zohoCliqRequest(tenantId, "POST", `/chats/${chatId}/pinmessages`, {
            body: {
              message_id: str(args.message_id ?? args.messageId),
              ...(args.expiry_time ? { expiry_time: Number(args.expiry_time) } : {}),
            },
          })
        );
      case "ZOHOCLIQ_LIST_PINNED_MESSAGES":
        return norm(await zohoCliqRequest(tenantId, "GET", `/chats/${chatId}/pinmessages`));
      case "ZOHOCLIQ_LIST_MY_PINS":
        return norm(await zohoCliqRequest(tenantId, "GET", "/mypins"));
      case "ZOHOCLIQ_API_REQUEST":
        return norm(
          await zohoCliqRequest(tenantId, str(args.method) || "GET", str(args.path) || "/", {
            ...(args.body && typeof args.body === "object" ? { body: args.body } : {}),
            ...(args.query && typeof args.query === "object"
              ? { query: args.query as Record<string, string> }
              : {}),
          })
        );
      default:
        return {
          ok: false,
          error: `unknown Zoho Cliq action ${action}. Known: ${Object.keys(ZOHO_CLIQ_ACTIONS).join(", ")}`,
        };
    }
  } catch (err: any) {
    const msg = err?.message ?? String(err);
    console.error(`[zohoCliq] action ${action} threw: ${msg}`);
    return { ok: false, error: msg };
  }
}

export function isZohoCliqAction(action: string): boolean {
  return action.toUpperCase().startsWith("ZOHOCLIQ_");
}

// Preserves the raw HTTP status on failure — callers (zohoCliqTools.ts) key
// their reconnect-vs-report decision off `status`, not the error string
// (Zoho's error text isn't a reliable signal: a 400/404 real-action problem
// and a 401 auth problem can both mention "invalid"/"error" in prose).
function norm(r: {
  ok: boolean;
  data?: unknown;
  error?: string;
  status?: number;
}): ZohoExecResult {
  if (r.ok) return { ok: true, data: r.data };
  console.error(`[zohoCliq] request failed status=${r.status ?? "?"} error=${r.error ?? "?"}`);
  return { ok: false, error: r.error, data: r.data, status: r.status };
}
