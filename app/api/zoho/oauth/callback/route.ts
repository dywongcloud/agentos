// app/api/zoho/oauth/callback/route.ts
//
// Zoho OAuth redirect target: https://<host>/api/zoho/oauth/callback
// Validates the single-use state nonce (minted by zohoCliqAuthUrl, carries the
// tenant + chat to notify), exchanges the code at the DC Zoho names in the
// `accounts-server` param (multi-datacenter), persists tokens per tenant, and
// pings the originating chat so the user knows the connection landed.

import { handleZohoCallback } from "@/app/lib/zohoCliq";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { recordActivity } from "@/app/lib/activityLog";
import type { Channel } from "@/app/lib/identity";

function html(title: string, body: string, status = 200): Response {
  return new Response(
    `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title>` +
      `<style>body{font-family:system-ui;margin:15vh auto;max-width:28rem;text-align:center;color:#222}</style>` +
      `</head><body><h2>${title}</h2><p>${body}</p></body></html>`,
    { status, headers: { "content-type": "text/html; charset=utf-8" } }
  );
}

export async function GET(req: Request): Promise<Response> {
  const url = new URL(req.url);
  const error = url.searchParams.get("error");
  if (error) {
    return html("Zoho connection cancelled", `Zoho reported: ${error}. You can close this tab and try again from chat.`, 400);
  }
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  if (!code || !state) {
    return html("Invalid request", "Missing code/state. Restart the connect flow from chat.", 400);
  }

  const res = await handleZohoCallback({
    code,
    state,
    accountsServer: url.searchParams.get("accounts-server"),
  });
  if (!res.ok) {
    return html("Zoho connection failed", `${res.error}. Ask the bot for a fresh connect link and try again.`, 400);
  }

  await recordActivity(res.tenantId, {
    kind: "tool",
    summary: "zoho cliq connected via OAuth",
    meta: { integration: "zohocliq" },
  });

  // Tell the originating chat (best-effort) so the flow closes the loop.
  if (res.channel && res.sessionId) {
    try {
      await sendOutboundRuntime({
        channel: res.channel as Channel,
        sessionId: res.sessionId,
        text: "✅ Zoho Cliq is connected. You can now ask me to read/send Cliq messages, schedule messages, manage pins, or set up Cliq triggers.",
      });
    } catch {
      // the HTML confirmation below still tells the user
    }
  }

  return html("Zoho Cliq connected ✅", "You can close this tab and return to your chat.");
}
