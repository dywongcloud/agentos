// app/lib/debugMode.ts
//
// Per-tenant debug-mode toggle.
//
// When OFF (default): the bot behaves like a human chat partner —
//   typing indicator while it thinks, then the final response. No
//   "Thinking… / Working… / Preparing response…" placeholders, no
//   tool-call narration.
//
// When ON: the existing verbose streaming kicks in — interim status
//   messages, live tool-call indicators, step counters.
//
// Toggled via the /debug Telegram command. Stored in Redis with no TTL —
// debug state sticks until the user changes it.

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";

function key(tenantId: string): string {
  return `debug:${tenantId}`;
}

export async function isDebugMode(tenantId: string): Promise<boolean> {
  // Allow env override for default. BOT_DEBUG_DEFAULT=true → debug on for
  // tenants that haven't opted out. Useful in dev / single-user deployments.
  const envDefault = env("BOT_DEBUG_DEFAULT") === "true";

  const store = getStore();
  try {
    const v = await store.get<string>(key(tenantId));
    if (v === "1") return true;
    if (v === "0") return false;
  } catch {
    // Fall through to env default on read failure.
  }
  return envDefault;
}

export async function setDebugMode(
  tenantId: string,
  enabled: boolean
): Promise<void> {
  const store = getStore();
  await store.set(key(tenantId), enabled ? "1" : "0");
}
