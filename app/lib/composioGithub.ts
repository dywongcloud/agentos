// app/lib/composioGithub.ts
//
// Pull a Composio-managed GitHub OAuth token for a tenant. Used by the
// claude-code repo flow so the agent can clone, commit, and push against
// the user's GitHub on their behalf, without us ever asking for a PAT in
// chat.
//
// Why Composio: the user has it wired (`COMPOSIO_API_KEY`) and already
// uses it for tool-calling. Composio stores OAuth tokens per (entity,
// toolkit) and refreshes them automatically. We just retrieve.
//
// Returns:
//   - { token, accountId }   when the tenant has an ACTIVE GitHub OAuth
//                            connection in Composio (`authScheme=OAUTH2`,
//                            `val.status=ACTIVE`, `val.access_token` set).
//   - null                   otherwise (no connection / wrong scheme / not
//                            yet active). Callers should surface a friendly
//                            "authorize GitHub in Composio" message.

import { Composio } from "@composio/core";
import { env } from "@/app/lib/env";

export type GithubConnection = {
  token: string;
  accountId: string;
  // Best-effort GitHub username if Composio surfaced it. Optional.
  username?: string;
};

let composioClientPromise: Promise<Composio | null> | null = null;

async function getComposioClient(): Promise<Composio | null> {
  if (composioClientPromise) return composioClientPromise;
  composioClientPromise = (async () => {
    const apiKey = env("COMPOSIO_API_KEY");
    if (!apiKey) return null;
    return new Composio({ apiKey });
  })();
  return composioClientPromise;
}

// Walk into the Composio ConnectionData union and pull the bearer token
// out of an ACTIVE OAuth2 connection. Returns null for any other shape.
function extractAccessToken(state: unknown): string | null {
  if (!state || typeof state !== "object") return null;
  const s = state as { authScheme?: string; val?: unknown };
  if (s.authScheme !== "OAUTH2" && s.authScheme !== "OAUTH1") return null;
  const val = s.val as { status?: string; access_token?: string } | undefined;
  if (!val || val.status !== "ACTIVE") return null;
  if (typeof val.access_token !== "string" || !val.access_token) return null;
  return val.access_token;
}

export async function getGithubTokenForTenant(
  tenantId: string
): Promise<GithubConnection | null> {
  const composio = await getComposioClient();
  if (!composio) return null;

  try {
    // Composio's list filter expects userIds (the entity id, matching what we
    // pass everywhere else as `userId`/tenantId in agentTurn).
    const resp = await composio.connectedAccounts.list({
      userIds: [tenantId],
      toolkitSlugs: ["github"],
      statuses: ["ACTIVE"],
      limit: 5,
    });

    const items = (resp as { items?: unknown[] }).items ?? [];
    for (const raw of items) {
      const item = raw as {
        id?: string;
        state?: unknown;
        toolkit?: { slug?: string };
      };
      if (item.toolkit?.slug !== "github") continue;
      const token = extractAccessToken(item.state);
      if (token && item.id) {
        return { token, accountId: item.id };
      }
    }
  } catch (err) {
    // Don't throw — the caller treats null as "user needs to connect GitHub"
    // and emits a friendly setup message. Log so we can debug.
    console.warn(
      `[composioGithub] list failed for tenant=${tenantId}: ${(err as Error)?.message ?? String(err)}`
    );
  }

  return null;
}
