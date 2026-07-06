import { NextResponse } from "next/server";
import { env } from "@/app/lib/env";
import { makeUiToken, setUiCookie } from "@/app/lib/uiAuth";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function safeNext(next: string | null | undefined): string {
  // Only same-origin paths — never redirect to an absolute URL from input.
  return next && next.startsWith("/") && !next.startsWith("//") ? next : "/ui";
}

export async function POST(req: Request) {
  const form = await req.formData();
  const password = String(form.get("password") ?? "");
  const next = safeNext(form.get("next") ? String(form.get("next")) : null);

  const expected = env("ADMIN_UI_PASSWORD");
  if (!expected) return new Response("Set ADMIN_UI_PASSWORD first.", { status: 500 });

  const url = new URL(req.url);
  const base = (env("APP_BASE_URL") ?? `${url.protocol}//${url.host}`).replace(/\/$/, "");

  if (password !== expected) {
    return NextResponse.redirect(
      `${base}/ui/login?error=1${next !== "/ui" ? `&next=${encodeURIComponent(next)}` : ""}`,
      303
    );
  }

  const token = makeUiToken();
  const res = NextResponse.redirect(`${base}${next}`, 303);
  setUiCookie(res, token);
  return res;
}

// Magic-link login: GET /api/ui/login?key=<ADMIN_UI_PASSWORD>&next=/ui/agents
// Sets the auth cookie and redirects — makes dashboard links work from the
// Telegram in-app browser (separate cookie jar, no saved session). Owner has
// explicitly accepted password-in-URL for this single-user deployment.
export async function GET(req: Request) {
  const url = new URL(req.url);
  const key = url.searchParams.get("key") ?? "";
  const next = safeNext(url.searchParams.get("next"));

  const expected = env("ADMIN_UI_PASSWORD");
  if (!expected) return new Response("Set ADMIN_UI_PASSWORD first.", { status: 500 });

  const base = (env("APP_BASE_URL") ?? `${url.protocol}//${url.host}`).replace(/\/$/, "");

  if (key !== expected) {
    return NextResponse.redirect(
      `${base}/ui/login${next !== "/ui" ? `?next=${encodeURIComponent(next)}` : ""}`,
      303
    );
  }

  const res = NextResponse.redirect(`${base}${next}`, 303);
  setUiCookie(res, makeUiToken());
  return res;
}
