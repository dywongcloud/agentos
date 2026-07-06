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

// ---------------------------------------------------------------------------
// Module-level singleton cache
// ---------------------------------------------------------------------------

type ComposioTools = {
  execute: (
    slug: string,
    body: Record<string, unknown>
  ) => Promise<{ data?: unknown; successful?: boolean; error?: unknown }>;
};

type ComposioClientCache = {
  apiKey: string;
  composio: unknown;
  tools: ComposioTools;
};

let _clientCache: ComposioClientCache | null = null;

async function getComposioClient(apiKey: string): Promise<ComposioTools> {
  if (_clientCache && _clientCache.apiKey === apiKey) {
    return _clientCache.tools;
  }
  const { Composio } = await import("@composio/core");
  const composio = new Composio({ apiKey });
  const tools = composio.tools as unknown as ComposioTools;
  _clientCache = { apiKey, composio, tools };
  return tools;
}

// ---------------------------------------------------------------------------
// Retry helpers
// ---------------------------------------------------------------------------

const RETRY_DELAYS_MS = [500, 1500, 4500];
const NON_RETRYABLE_PATTERNS = [
  "not found",
  "invalid",
  "unauthorized",
  "forbidden",
  "bad request",
];

function isRetryable(message: string): boolean {
  const lower = message.toLowerCase();
  return !NON_RETRYABLE_PATTERNS.some((p) => lower.includes(p));
}

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ---------------------------------------------------------------------------
// Main executor
// ---------------------------------------------------------------------------

export async function executeComposioAction(
  tenantId: string,
  action: string,
  args: Record<string, unknown>,
  connectedAccountId?: string
): Promise<ComposioExecResult> {
  const apiKey = env("COMPOSIO_API_KEY");
  if (!apiKey) return { ok: false, error: "COMPOSIO_API_KEY not configured" };

  const maxAttempts = 3;
  let lastError = "";

  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    if (attempt > 0) {
      await sleep(RETRY_DELAYS_MS[attempt - 1]);
    }

    try {
      const tools = await getComposioClient(apiKey);
      const resp = await tools.execute(action, {
        userId: tenantId,
        arguments: args,
        dangerouslySkipVersionCheck: true,
        ...(connectedAccountId ? { connectedAccountId } : {}),
      });
      if (resp && resp.successful === false) {
        const errMsg = String(resp.error ?? "action failed");
        if (!isRetryable(errMsg) || attempt === maxAttempts - 1) {
          return { ok: false, error: errMsg, data: resp.data };
        }
        lastError = errMsg;
        continue;
      }
      return { ok: true, data: resp?.data ?? null };
    } catch (err: any) {
      const errMsg: string = err?.message ?? String(err);
      if (!isRetryable(errMsg) || attempt === maxAttempts - 1) {
        return { ok: false, error: errMsg };
      }
      lastError = errMsg;
    }
  }

  return { ok: false, error: lastError };
}
