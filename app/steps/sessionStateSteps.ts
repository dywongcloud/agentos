// app/steps/sessionStateSteps.ts
import type { ModelMessage } from "ai";
import { loadTgMessage, saveTgMessage } from "@/app/lib/tgMessageMap";

const historyKey = (sessionId: string) => `sess:${sessionId}:history`;

// Resolves a Telegram reply binding into a grounding string the chat agent
// can use to anchor its response. When the user uses Telegram's explicit
// reply gesture, `replyToTgMessageId` points at one of OUR earlier
// assistant messages — looking that up here gives the model an
// unambiguous thread anchor that no system prompt rule can outweigh.
// Returns `null` when there's nothing to bind to (no reply gesture used,
// or the bot message has aged out of the side-table's 7-day TTL).
//
// Also saves the inbound user message into the side table so the next
// turn can recover it the same way (covers self-reply patterns and any
// future feature that needs to walk the thread).
export async function resolveReplyContextStep(args: {
  sessionId: string;
  tgMessageId?: number;
  replyToTgMessageId?: number;
  text: string;
}): Promise<{ groundingTag: string | null }> {
  "use step";
  if (args.tgMessageId) {
    try {
      await saveTgMessage({
        sessionId: args.sessionId,
        messageId: args.tgMessageId,
        role: "user",
        text: args.text,
      });
    } catch {
      // best-effort
    }
  }
  if (!args.replyToTgMessageId) return { groundingTag: null };
  try {
    const bound = await loadTgMessage(args.sessionId, args.replyToTgMessageId);
    if (!bound || bound.role !== "assistant") return { groundingTag: null };
    // Trim to a reasonable size — the agent's context window matters
    // more than perfect fidelity, and the binding is unambiguous either way.
    const preview = bound.text.length > 1200
      ? bound.text.slice(0, 1200) + "…"
      : bound.text;
    return {
      groundingTag: `[user is explicitly replying via Telegram's reply gesture to your earlier message: "${preview}"]`,
    };
  } catch {
    return { groundingTag: null };
  }
}

export async function loadHistoryStep(sessionId: string): Promise<ModelMessage[]> {
  "use step";

  const { Redis } = await import("@upstash/redis");

  const url = process.env.KV_REST_API_URL ?? process.env.UPSTASH_REDIS_REST_URL;
  const token = process.env.KV_REST_API_TOKEN ?? process.env.UPSTASH_REDIS_REST_TOKEN;

  if (!url || !token) return [];

  const redis = new Redis({ url, token });
  return (await redis.get(historyKey(sessionId))) ?? [];
}

export async function saveHistoryStep(sessionId: string, history: ModelMessage[]) {
  "use step";

  const { Redis } = await import("@upstash/redis");

  const url = process.env.KV_REST_API_URL ?? process.env.UPSTASH_REDIS_REST_URL;
  const token = process.env.KV_REST_API_TOKEN ?? process.env.UPSTASH_REDIS_REST_TOKEN;

  if (!url || !token) return;

  const redis = new Redis({ url, token });
  await redis.set(historyKey(sessionId), history);
}
