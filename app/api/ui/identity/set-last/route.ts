import { NextResponse } from "next/server";
import { env } from "@/app/lib/env";
import { getUiCookie, verifyUiToken } from "@/app/lib/uiAuth";
import { saveSessionMeta } from "@/app/lib/sessionMeta";
import type { Channel } from "@/app/lib/identity";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// Force the dashboard's default identity (Redis `last:any`) to a specific
// userId, e.g. "telegram:1236381479". The UI derives the shown account from
// the last allowed inbound message; this lets an admin pin it back without
// having to message the bot.
export async function POST(req: Request) {
  const ok = verifyUiToken(await getUiCookie());
  if (!ok) return new Response("Unauthorized", { status: 401 });

  const form = await req.formData();
  const raw = String(form.get("userId") ?? "").trim();
  // userId looks like "<channel>:<id>", e.g. "telegram:1236381479".
  const m = /^([a-z]+):(.+)$/i.exec(raw);
  if (!m) {
    return new Response(
      `Invalid userId "${raw}". Expected "<channel>:<id>", e.g. telegram:1236381479.`,
      { status: 400 }
    );
  }
  const channel = m[1].toLowerCase() as Channel;
  const id = m[2];
  const sessionId = `${channel}:${id}`;

  await saveSessionMeta(
    {
      channel,
      sessionId,
      senderId: id,
      updatedAt: Date.now(),
    },
    { updateLast: true }
  );

  const url = new URL(req.url);
  const baseUrl = env("APP_BASE_URL") ?? `${url.protocol}//${url.host}`;
  return NextResponse.redirect(
    `${baseUrl.replace(/\/$/, "")}/ui?userId=${encodeURIComponent(sessionId)}`,
    303
  );
}
