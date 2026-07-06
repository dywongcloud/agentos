// app/lib/composioTriggers.ts
//
// Subscribe / unsubscribe / list Composio triggers (event subscriptions on
// connected integrations: new email, GitHub PR, Slack message, etc.).
//
// Wraps composio.triggers — kept in one place so the rest of the codebase
// doesn't have to worry about Composio's API surface.
//
// We also maintain a small Redis index `trigger:instance:{triggerId} = tenantId`
// so the webhook receiver can find the right Telegram chat to notify when an
// event fires. Composio's webhook payload includes the trigger instance id,
// not our tenantId, so the lookup is necessary.

import { Composio } from "@composio/core";

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import { recordAudit } from "@/app/lib/auditLog";

export type TriggerSubscription = {
  triggerId: string; // Composio's instance id
  triggerName: string; // e.g. "GITHUB_PULL_REQUEST_EVENT"
  connectedAccountId: string;
  createdAt: number;
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

// Subscribe a tenant to a trigger slug (e.g. "GITHUB_PULL_REQUEST_EVENT").
// Returns the created instance id, or { error } if Composio rejected.
//
// `triggerConfig` is the trigger-specific config (e.g. { owner: "foo", repo:
// "bar" } for GitHub). Caller is responsible for figuring out which keys are
// required; the agent's recommended UX is to call list_available_triggers
// first to inspect the config schema, then call this with the right keys.
export async function subscribeTrigger(args: {
  tenantId: string;
  slug: string;
  triggerConfig?: Record<string, unknown>;
  connectedAccountId?: string;
  toolkit?: string;
}): Promise<{ ok: true; triggerId: string } | { ok: false; error: string }> {
  const composio = await getComposio();
  if (!composio) return { ok: false, error: "COMPOSIO_API_KEY not configured" };

  // Pin an ACTIVE connected account. When the caller doesn't pass one, Composio
  // picks for us — and with several stale/EXPIRED duplicate accounts on a
  // toolkit it routinely binds a dead one, failing with
  // TOOL_AUTH_BadConnectedAccountState even though an ACTIVE account exists.
  // Resolve the ACTIVE account ourselves first. Toolkit defaults to the slug's
  // leading token (GMAIL_NEW_GMAIL_MESSAGE → gmail).
  let connectedAccountId = args.connectedAccountId;
  if (!connectedAccountId) {
    const toolkit = args.toolkit ?? args.slug.split("_")[0] ?? args.slug;
    try {
      const { isToolkitConnected } = await import(
        "@/app/lib/composioConnections"
      );
      const conn = await isToolkitConnected(args.tenantId, toolkit);
      if (conn.connected && conn.accountId) {
        connectedAccountId = conn.accountId;
      } else if (!conn.connected) {
        return {
          ok: false,
          error:
            `${toolkit} has no ACTIVE connection` +
            `${conn.status ? ` (status: ${conn.status})` : ""}. ` +
            `Reconnect ${toolkit}, then resume the rule.`,
        };
      }
    } catch {
      // Resolver failed — fall through and let Composio choose.
    }
  }

  try {
    const result = await composio.triggers.create(args.tenantId, args.slug, {
      ...(connectedAccountId ? { connectedAccountId } : {}),
      ...(args.triggerConfig ? { triggerConfig: args.triggerConfig } : {}),
    } as Record<string, unknown> as never);

    const triggerId =
      (result as { triggerId?: string; id?: string }).triggerId ??
      (result as { triggerId?: string; id?: string }).id ??
      "";
    if (!triggerId) {
      return { ok: false, error: "Composio returned no trigger id" };
    }

    // Map triggerId → tenantId so the webhook receiver can find the user.
    const store = getStore();
    await store.set(`trigger:instance:${triggerId}`, args.tenantId);

    await recordAudit(args.tenantId, {
      kind: "trigger.subscribed",
      summary: `subscribed to ${args.slug}`,
      after: "subscribed",
      meta: {
        slug: args.slug,
        triggerId,
        connectedAccountId,
      },
    });

    return { ok: true, triggerId };
  } catch (err: any) {
    return { ok: false, error: err?.message ?? String(err) };
  }
}

export async function unsubscribeTrigger(
  triggerId: string
): Promise<{ ok: boolean; error?: string }> {
  const composio = await getComposio();
  if (!composio) return { ok: false, error: "COMPOSIO_API_KEY not configured" };
  try {
    // Recover tenantId from the Redis mapping BEFORE we delete it so the
    // audit entry is scoped to the right tenant.
    const store = getStore();
    const tenantId =
      (await store.get<string>(`trigger:instance:${triggerId}`)) ?? "";
    await composio.triggers.disable(triggerId);
    await store.del(`trigger:instance:${triggerId}`);
    if (tenantId) {
      await recordAudit(tenantId, {
        kind: "trigger.unsubscribed",
        summary: `unsubscribed trigger ${triggerId}`,
        before: "subscribed",
        meta: { triggerId },
      });
    }
    return { ok: true };
  } catch (err: any) {
    return { ok: false, error: err?.message ?? String(err) };
  }
}

export async function listSubscriptions(
  tenantId: string
): Promise<TriggerSubscription[]> {
  const composio = await getComposio();
  if (!composio) return [];
  try {
    const resp = await composio.triggers.listActive({
      // The SDK supports userIds for filtering; pass our channel-qualified id.
      // If Composio doesn't return anything for this id (e.g. when the user
      // never subscribed), we just get an empty list.
      // @ts-expect-error - userIds isn't on every SDK version, but is supported
      userIds: [tenantId],
      limit: 100,
    });
    const items = (resp as { items?: unknown[] }).items ?? [];
    const out: TriggerSubscription[] = [];
    for (const raw of items) {
      const it = raw as {
        id: string;
        triggerName: string;
        connectedAccountId: string;
        updatedAt?: string;
      };
      out.push({
        triggerId: it.id,
        triggerName: it.triggerName,
        connectedAccountId: it.connectedAccountId,
        createdAt: it.updatedAt ? Date.parse(it.updatedAt) : Date.now(),
      });
    }
    return out;
  } catch {
    return [];
  }
}

export async function tenantForTriggerInstance(
  triggerId: string
): Promise<string | null> {
  const store = getStore();
  return (await store.get<string>(`trigger:instance:${triggerId}`)) ?? null;
}
