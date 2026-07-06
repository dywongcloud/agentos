import type { Channel } from "@/app/lib/identity";
import { telegramSendMessage } from "@/app/lib/providers/telegram";
import { getTextbeltReplyWebhookUrl, textbeltSendSms } from "./providers/textbelt";
//import { whatsappSendMessage, whatsappSessionToTo } from "@/app/lib/providers/whatsapp";
//import { getTextbeltReplyWebhookUrl, textbeltSendSms } from "@/app/lib/providers/textbelt";

// Telegram's hard cap is 4096 chars per text message. Keep some headroom for
// HTML entity escaping and chunk markers so we never hit "message is too long".
const TELEGRAM_SAFE_CHUNK = 3800;

// Split text into Telegram-safe chunks, preferring to break on blank lines /
// newlines so multi-paragraph output reads naturally. Each chunk respects
// TELEGRAM_SAFE_CHUNK as a hard upper bound.
function splitForTelegram(text: string): string[] {
  if (!text) return [];
  if (text.length <= TELEGRAM_SAFE_CHUNK) return [text];

  const out: string[] = [];
  let pos = 0;
  while (pos < text.length) {
    let end = Math.min(text.length, pos + TELEGRAM_SAFE_CHUNK);
    // If we're not at end-of-text, look for a clean break — paragraph,
    // then sentence, then any newline, then any space. Anything past
    // halfway through the chunk is fine.
    if (end < text.length) {
      const halfway = pos + Math.floor(TELEGRAM_SAFE_CHUNK / 2);
      let bp = text.lastIndexOf("\n\n", end);
      if (bp < halfway) bp = text.lastIndexOf("\n", end);
      if (bp < halfway) bp = text.lastIndexOf(". ", end);
      if (bp < halfway) bp = text.lastIndexOf(" ", end);
      if (bp >= halfway) end = bp + 1;
    }
    out.push(text.slice(pos, end));
    pos = end;
  }
  return out;
}

/**
 * Runtime outbound send helper.
 * Safe to call from:
 * - Route handlers (webhooks)
 * - Workflow steps
 *
 * Long Telegram messages are auto-chunked into Telegram-safe pieces and sent
 * sequentially. This fixes the "deep job finished but never delivered"
 * failure mode where the resultText exceeded Telegram's 4096-char cap and
 * sendMessage 400'd, leaving the user staring at silence.
 */
export async function sendOutboundRuntime(args: { channel: Channel; sessionId: string; text: string; baseUrlHint?: string }) {
  const { channel, sessionId, text, baseUrlHint } = args;

  if (channel === "telegram") {
    const chunks = splitForTelegram(text ?? "");
    if (chunks.length === 0) return;
    for (let i = 0; i < chunks.length; i++) {
      // Only notify on the first chunk so the user gets one ping for a
      // multi-part message, not N. Continuation chunks come in silently.
      const messageId = await telegramSendMessage(sessionId, chunks[i], {
        disableNotification: i > 0,
      });
      // Persist the (sessionId, messageId) → text mapping so when the user
      // uses Telegram's reply gesture on this message later, the inbound
      // webhook handler can recover the exact text they're replying to.
      // Best-effort: storage failures here must NOT block delivery.
      try {
        const { saveTgMessage } = await import("@/app/lib/tgMessageMap");
        await saveTgMessage({
          sessionId,
          messageId,
          role: "assistant",
          text: chunks[i],
        });
      } catch {
        // ignore
      }
    }
    return;
  }
/*
  if (channel === "whatsapp") {
    const to = whatsappSessionToTo(sessionId);
    if (!to) throw new Error(`Invalid whatsapp sessionId: ${sessionId}`);
    await whatsappSendMessage(to, text);
    return;
  }
*/

  if (channel === "sms") {
    const to = sessionId.split(":")[1] ?? "";
    if (!to) throw new Error(`Invalid sms sessionId: ${sessionId}`);

    // Include reply webhook so the user can reply back to the bot (US numbers only, paid key required)
    const replyWebhookUrl = getTextbeltReplyWebhookUrl(baseUrlHint);

    const resp = await textbeltSendSms({
      to,
      message: text,
      replyWebhookUrl,
    });

    if (!resp.success) {
      throw new Error(`Textbelt send failed: ${resp.error ?? "unknown error"}`);
    }
    return;
  }

  throw new Error(`Unsupported channel: ${channel}`);
}
