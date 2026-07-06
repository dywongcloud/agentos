// app/lib/uiRequire.ts
import { redirect } from "next/navigation";
import { getUiCookie, verifyUiToken } from "@/app/lib/uiAuth";

// Rebuild the page's own URL (path + string search params) so the login
// redirect can round-trip back to it via ?next=.
export function uiPathWithQuery(path: string, sp: Record<string, unknown>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(sp)) if (typeof v === "string") q.set(k, v);
  const s = q.toString();
  return s ? `${path}?${s}` : path;
}

export async function requireUiAuthPage(next?: string) {
  const token = await getUiCookie();
  if (!verifyUiToken(token)) {
    redirect(next ? `/ui/login?next=${encodeURIComponent(next)}` : "/ui/login");
  }
}
