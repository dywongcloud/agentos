// app/lib/tenantPause.ts
//
// A per-tenant "pause everything" kill switch. When set, the single firing
// chokepoint (fireAutomation) and the workforce-stage runner bail out, so no
// automation, agent, or workforce does any work for that tenant — without
// deleting or disabling anything individually. Flip it back off to resume.
//
// Stored as one flag per tenant (mirrors accountObjective's single-blob shape).

import { getStore } from "@/app/lib/store";

const pauseKey = (tenantId: string) => `pause:tenant:${tenantId}`;

// Stored as a raw "1" string flag (mirrors the store's debug-flag convention):
// the Upstash decoder only JSON-parses values that look structural, so a bare
// boolean would round-trip as the string "true" and break a `=== true` check.
export async function isTenantPaused(tenantId: string): Promise<boolean> {
  if (!tenantId) return false;
  const val = await getStore().get<string>(pauseKey(tenantId));
  return val === "1";
}

export async function setTenantPaused(
  tenantId: string,
  paused: boolean
): Promise<boolean> {
  const store = getStore();
  if (paused) await store.set(pauseKey(tenantId), "1");
  else await store.del(pauseKey(tenantId));
  return paused;
}
