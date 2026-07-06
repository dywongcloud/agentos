// app/lib/composioExec.ts
//
// Minimal deterministic Composio action executor, shared by the custom-trigger
// poller and the dashboard widget executor. Mirrors the agentic tool path's
// version handling: manual execution MUST pass dangerouslySkipVersionCheck,
// otherwise Composio rejects every call with "Toolkit version not specified".
//
// Kept dependency-light (only the Composio SDK, lazily) so it's safe to import
// from the widget refresh path without dragging in delivery/session code.

import { env } from "@/app/lib/env";

export type ComposioExecResult = {
  ok: boolean;
  data?: unknown;
  error?: string;
};

export async function executeComposioAction(
  tenantId: string,
  action: string,
  args: Record<string, unknown>,
  connectedAccountId?: string
): Promise<ComposioExecResult> {
  const apiKey = env("COMPOSIO_API_KEY");
  if (!apiKey) return { ok: false, error: "COMPOSIO_API_KEY not configured" };

  try {
    const { Composio } = await import("@composio/core");
    const composio = new Composio({ apiKey });
    const resp = await (composio.tools as unknown as {
      execute: (
        slug: string,
        body: Record<string, unknown>
      ) => Promise<{ data?: unknown; successful?: boolean; error?: unknown }>;
    }).execute(action, {
      userId: tenantId,
      arguments: args,
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
