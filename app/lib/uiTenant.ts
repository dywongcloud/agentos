// app/lib/uiTenant.ts
//
// Single source of truth for "which tenant is the UI showing". Every dashboard
// page/API used to inline its own resolveTenant() that defaulted to
// getLastSession("any") — i.e. whoever messaged the bot most recently. On a
// single-owner admin dashboard that's wrong: a stray inbound from another chat
// would hijack the default view. We default to the OWNER instead.
//
// Resolution order:
//   1. an explicit ?userId= (the user clicked into a specific tenant)
//   2. UI_DEFAULT_TENANT env (explicit override, e.g. "telegram:1236381479")
//   3. the first ADMIN_IDENTITIES entry (the dashboard owner)
//   4. last:any — the most recent session of anyone (legacy fallback)

import { csvEnv, env } from "@/app/lib/env";
import { getLastSession } from "@/app/lib/sessionMeta";

export async function resolveUiTenant(spUserId?: string | null): Promise<string | null> {
  if (spUserId) return spUserId;

  const override = env("UI_DEFAULT_TENANT");
  if (override) return override;

  const admins = csvEnv("ADMIN_IDENTITIES");
  if (admins.length > 0) return admins[0];

  const last = await getLastSession("any");
  if (!last) return null;
  const senderId = last.sessionId.split(":")[1] ?? last.sessionId;
  return `${last.channel}:${senderId}`;
}
