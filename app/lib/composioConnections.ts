// app/lib/composioConnections.ts
//
// Helpers around Composio's connectedAccounts surface that the
// natural-language subscription flow needs:
//
//   listConnectedToolkits(tenantId)
//     → which toolkits is this user already connected to (slack, gmail, github, …)?
//       Used so the agent can short-circuit "do I need to start auth?" without
//       guessing.
//
//   initiateConnection(tenantId, toolkit, callbackUrl?)
//     → kick off OAuth (or whatever the toolkit needs) for a toolkit the user
//       isn't connected to yet. Returns the auth URL we can drop into chat.
//
//   listTriggerTypes({ toolkits, keyword })
//     → discover available triggers (with config schemas) given a toolkit
//       and an optional keyword. Used by the discover_triggers tool.
//
// Centralizing here keeps the tool code thin and lets us swap Composio
// internals in one place if their SDK shapes shift.

import { Composio } from "@composio/core";
import { env } from "@/app/lib/env";
import { recordAudit } from "@/app/lib/auditLog";

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

export type ConnectedToolkitSummary = {
  toolkitSlug: string;
  toolkitName?: string;
  connectedAccountId: string;
  status: string;
};

// One live fetch of the tenant's connected accounts. THROWS on API failure so
// callers can tell a transient error apart from "genuinely nothing connected".
async function fetchConnectedAccounts(
  tenantId: string
): Promise<ConnectedToolkitSummary[]> {
  const composio = await getComposio();
  if (!composio) throw new Error("COMPOSIO_API_KEY not configured");
  const resp = await composio.connectedAccounts.list({
    userIds: [tenantId],
    limit: 100,
  } as never);
  const items =
    (resp as { items?: Array<Record<string, unknown>> }).items ?? [];
  const out: ConnectedToolkitSummary[] = [];
  for (const raw of items) {
    const it = raw as {
      id?: string;
      status?: string;
      toolkit?: { slug?: string; name?: string };
    };
    if (!it.id || !it.toolkit?.slug) continue;
    out.push({
      toolkitSlug: it.toolkit.slug,
      toolkitName: it.toolkit.name,
      connectedAccountId: it.id,
      status: String(it.status ?? "UNKNOWN"),
    });
  }
  return out;
}

// Retry the fetch a few times before giving up. Composio's list endpoint
// occasionally 5xx/times-out; a single transient failure used to be swallowed
// to an empty list, which the agent then read as "you're not connected / your
// connection expired" — a hallucination that frustrated users whose
// connection was actually fine. Retrying first removes most of those.
async function fetchConnectedAccountsWithRetry(
  tenantId: string,
  attempts = 3
): Promise<ConnectedToolkitSummary[]> {
  let lastErr: unknown;
  for (let i = 0; i < attempts; i++) {
    try {
      return await fetchConnectedAccounts(tenantId);
    } catch (err) {
      lastErr = err;
      if (i < attempts - 1) {
        await new Promise((r) => setTimeout(r, 200 * (i + 1)));
      }
    }
  }
  throw lastErr ?? new Error("failed to list connected accounts");
}

// Best-effort list: returns [] on total failure (for callers that only need a
// hint). Prefer isToolkitConnected() when a wrong answer would mislead a user.
export async function listConnectedToolkits(
  tenantId: string
): Promise<ConnectedToolkitSummary[]> {
  try {
    return await fetchConnectedAccountsWithRetry(tenantId);
  } catch {
    return [];
  }
}

// Models name toolkits inconsistently: "google_calendar", "Google Calendar",
// "googlecalendar", "gcal". Composio stores a single canonical slug
// ("googlecalendar"). Strip everything but [a-z0-9] so "google_calendar" and
// "googlecalendar" collide, and fold a few well-known aliases to their
// canonical Composio slug so an exact-string mismatch never reads as
// "not connected".
const TOOLKIT_ALIASES: Record<string, string> = {
  googlecalendar: "googlecalendar",
  gcal: "googlecalendar",
  gcalendar: "googlecalendar",
  calendar: "googlecalendar",
  googledrive: "googledrive",
  gdrive: "googledrive",
  googlemail: "gmail",
  email: "gmail",
  googlesheets: "googlesheets",
  gsheets: "googlesheets",
  googledocs: "googledocs",
  gdocs: "googledocs",
};
export function normalizeToolkitSlug(slug: string): string {
  const bare = (slug || "").toLowerCase().replace(/[^a-z0-9]/g, "");
  return TOOLKIT_ALIASES[bare] ?? bare;
}

export async function isToolkitConnected(
  tenantId: string,
  toolkitSlug: string
): Promise<{
  connected: boolean;
  accountId?: string;
  status?: string;
  matchedSlug?: string;
  alsoConnected?: string[];
  // True when an account for this toolkit EXISTS but no record is ACTIVE
  // (Composio labelled them EXPIRED/INACTIVE). Composio routinely refreshes
  // such tokens on the next execute, so this is "attempt anyway", NOT a
  // confirmed dead connection — callers must not preemptively claim expiry.
  stale?: boolean;
  // True when we could NOT verify because the Composio API errored — distinct
  // from a confirmed "not connected". Callers must NOT tell the user their
  // connection expired in this case.
  error?: boolean;
  errorMessage?: string;
}> {
  let all: ConnectedToolkitSummary[];
  try {
    all = await fetchConnectedAccountsWithRetry(tenantId);
  } catch (err: any) {
    return {
      connected: false,
      error: true,
      errorMessage: err?.message ?? String(err),
    };
  }
  const want = normalizeToolkitSlug(toolkitSlug);
  const sameToolkit = all.filter(
    (c) => normalizeToolkitSlug(c.toolkitSlug) === want
  );
  const active = sameToolkit.find((c) => /ACTIVE/i.test(c.status));
  // Treat the mere PRESENCE of an account as connected. The old code required
  // an ACTIVE status, but Composio leaves many working connections labelled
  // EXPIRED (it refreshes the OAuth token lazily on the next execute) — so the
  // ACTIVE-only gate made the agent declare live connections "expired" and
  // refuse to act. Prefer an ACTIVE record; fall back to any existing one and
  // mark it `stale` so the agent attempts the action and lets the real execute
  // call be the source of truth for auth failure.
  const usable = active ?? sameToolkit[0];
  if (usable) {
    return {
      connected: true,
      accountId: usable.connectedAccountId,
      status: usable.status,
      matchedSlug: usable.toolkitSlug,
      stale: !active,
    };
  }
  // No account at all for this toolkit. Surface the full list of what IS
  // connected, so the agent can self-correct when it guessed the wrong slug
  // instead of looping on auth.
  return {
    connected: false,
    alsoConnected: all
      .filter((c) => /ACTIVE/i.test(c.status))
      .map((c) => c.toolkitSlug),
  };
}

// Kick off an OAuth (or other auth scheme) flow for a toolkit. We use
// Composio's `connectedAccounts.initiate` which returns a redirect URL the
// user opens in their browser to authorize. The resulting connection is
// scoped to our `tenantId`, so subsequent tool calls + trigger subscriptions
// see it automatically.
//
// Composio needs an `authConfigId` for the toolkit. We try to resolve one
// dynamically from `authConfigs.list`; if no auth config is registered for
// this toolkit yet, we surface a helpful error so the user knows the bot
// owner needs to register one in the Composio dashboard.
export async function initiateConnection(args: {
  tenantId: string;
  toolkitSlug: string;
  callbackUrl?: string;
}): Promise<
  | { ok: true; authUrl: string; connectedAccountId?: string }
  | { ok: false; error: string }
> {
  const composio = await getComposio();
  if (!composio) {
    return { ok: false, error: "COMPOSIO_API_KEY not configured" };
  }
  try {
    // 1. Find an authConfig for this toolkit.
    const cfgs = await (composio as unknown as {
      authConfigs: {
        list: (q: unknown) => Promise<{ items?: Array<{ id?: string }> }>;
      };
    }).authConfigs.list({
      toolkit: args.toolkitSlug,
      limit: 5,
    });
    const authConfigId = cfgs.items?.[0]?.id;
    if (!authConfigId) {
      return {
        ok: false,
        error: `No auth config registered for toolkit "${args.toolkitSlug}". The bot owner needs to add one in the Composio dashboard.`,
      };
    }

    // 2. Initiate. Composio returns a redirect URL we hand back as a chat link.
    const resp = await (composio as unknown as {
      connectedAccounts: {
        initiate: (
          userId: string,
          authConfigId: string,
          opts?: Record<string, unknown>
        ) => Promise<{ redirectUrl?: string; id?: string }>;
      };
    }).connectedAccounts.initiate(args.tenantId, authConfigId, {
      ...(args.callbackUrl ? { callbackUrl: args.callbackUrl } : {}),
      // A tenant can accumulate stale/expired connected accounts for the same
      // toolkit (e.g. an old EXPIRED Gmail link). Without this, initiate throws
      // "Multiple connected accounts found ... use the allowMultiple option"
      // and we can't even hand the user a reconnect link. Allowing multiple
      // lets a fresh ACTIVE connection be created alongside the dead ones;
      // isToolkitConnected() then matches the first ACTIVE record.
      allowMultiple: true,
    });

    if (!resp.redirectUrl) {
      return { ok: false, error: "Composio did not return a redirect URL" };
    }
    // Mark the OAuth flow start in the audit log. The drift detector picks
    // up the eventual ACTIVE/EXPIRED transitions on the next audit read.
    await recordAudit(args.tenantId, {
      kind: "integration.refreshed",
      summary: `started ${args.toolkitSlug} OAuth flow`,
      after: "INITIATED",
      meta: {
        toolkit: args.toolkitSlug,
        connectedAccountId: resp.id,
        authConfigId,
      },
    });
    return {
      ok: true,
      authUrl: resp.redirectUrl,
      connectedAccountId: resp.id,
    };
  } catch (err: any) {
    return { ok: false, error: err?.message ?? String(err) };
  }
}

// List trigger types, optionally filtered to a set of toolkits, with an
// optional keyword filter applied client-side (Composio's list endpoint
// doesn't currently support full-text search).
export type TriggerTypeSummary = {
  slug: string;
  name: string;
  description: string;
  toolkitSlug?: string;
  toolkitName?: string;
  configSchema?: unknown;
};

function keywordRank(
  list: TriggerTypeSummary[],
  keyword?: string
): TriggerTypeSummary[] {
  if (!keyword) return list;
  const k = keyword.toLowerCase();
  return list
    .map((t) => {
      const score =
        (t.slug.toLowerCase().includes(k) ? 3 : 0) +
        (t.name.toLowerCase().includes(k) ? 2 : 0) +
        (t.description.toLowerCase().includes(k) ? 1 : 0);
      return { t, score };
    })
    .filter((x) => x.score > 0)
    .sort((a, b) => b.score - a.score)
    .map((x) => x.t);
}

export async function listTriggerTypes(args: {
  toolkits?: string[];
  keyword?: string;
  limit?: number;
}): Promise<TriggerTypeSummary[]> {
  const composio = await getComposio();
  if (!composio) {
    console.warn("[composioConnections] listTriggerTypes: no COMPOSIO_API_KEY");
    return [];
  }
  const limit = args.limit ?? 20;
  const toolkitsLc = (args.toolkits ?? []).map((t) => t.toLowerCase());

  // --- Primary path: structured listTypes with full config schemas ---------
  try {
    const params: Record<string, unknown> = {};
    if (toolkitsLc.length > 0) params.toolkits = toolkitsLc;
    params.limit = Math.min(200, Math.max(limit, 50));
    const resp = await composio.triggers.listTypes(params as never);
    const items =
      (resp as { items?: Array<Record<string, unknown>> }).items ?? [];
    console.log(
      `[composioConnections] listTypes toolkits=${JSON.stringify(toolkitsLc)} → ${items.length} items`
    );
    let out: TriggerTypeSummary[] = [];
    for (const raw of items) {
      const it = raw as {
        slug?: string;
        name?: string;
        description?: string;
        config?: unknown;
        payload?: unknown;
        toolkit?: { slug?: string; name?: string };
      };
      if (!it.slug || !it.name) continue;
      out.push({
        slug: it.slug,
        name: it.name,
        description: String(it.description ?? ""),
        toolkitSlug: it.toolkit?.slug,
        toolkitName: it.toolkit?.name,
        configSchema: it.config ?? it.payload,
      });
    }
    out = keywordRank(out, args.keyword ?? undefined);
    if (out.length > 0) return out.slice(0, limit);
    console.warn(
      "[composioConnections] listTypes returned 0 usable items — falling back to listEnum"
    );
  } catch (err: any) {
    console.warn(
      `[composioConnections] listTypes threw, falling back to listEnum: ${err?.message ?? String(err)}`
    );
  }

  // --- Fallback path: enum of ALL trigger slugs (CLI path, no filters) ------
  // listTypes can come back empty when the project's toolkit/auth-config
  // scoping hides catalog entries. listEnum returns the raw slug list, which
  // we filter by toolkit-prefix + keyword ourselves, then hydrate the top few
  // with getType() to recover config schemas.
  try {
    const enumResp = await (composio as unknown as {
      triggers: { listEnum: () => Promise<string[]> };
    }).triggers.listEnum();
    const allSlugs = Array.isArray(enumResp) ? enumResp : [];
    console.log(`[composioConnections] listEnum → ${allSlugs.length} slugs`);

    let candidates = allSlugs;
    if (toolkitsLc.length > 0) {
      candidates = candidates.filter((slug) => {
        const s = slug.toLowerCase();
        return toolkitsLc.some((tk) => s.startsWith(tk + "_") || s.includes(tk));
      });
    }
    if (args.keyword) {
      const k = args.keyword.toLowerCase();
      const kw = candidates.filter((s) => s.toLowerCase().includes(k));
      if (kw.length > 0) candidates = kw;
    }
    candidates = candidates.slice(0, Math.min(limit, 12));

    // Hydrate top candidates with full type info (name/description/config).
    const out: TriggerTypeSummary[] = [];
    for (const slug of candidates) {
      try {
        const t = (await composio.triggers.getType(slug)) as {
          slug?: string;
          name?: string;
          description?: string;
          config?: unknown;
          payload?: unknown;
          toolkit?: { slug?: string; name?: string };
        };
        out.push({
          slug: t.slug ?? slug,
          name: t.name ?? slug,
          description: String(t.description ?? ""),
          toolkitSlug: t.toolkit?.slug,
          toolkitName: t.toolkit?.name,
          configSchema: t.config ?? t.payload,
        });
      } catch {
        // Couldn't hydrate — still surface the bare slug so the agent can try.
        out.push({ slug, name: slug, description: "" });
      }
    }
    return out;
  } catch (err: any) {
    console.warn(
      `[composioConnections] listEnum fallback failed: ${err?.message ?? String(err)}`
    );
    return [];
  }
}
