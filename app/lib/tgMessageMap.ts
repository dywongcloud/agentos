// app/lib/tgMessageMap.ts
//
// Side table mapping Telegram (chat_id, message_id) → assistant text we sent.
// Powers reply-threading: when a user uses Telegram's reply gesture on one of
// our messages, the inbound webhook carries `reply_to_message.message_id`,
// and we use this map to recover the exact text they replied to so the chat
// agent can ground its response in the right thread instead of guessing
// against history.
//
// Why a side table instead of stamping IDs onto the existing ChatMessage
// history: the history is normalized to AI-SDK `ModelMessage` for the chat
// model and pruned to ~30 entries. We don't want to bloat that shape with
// per-channel metadata, and we also want a longer retention window than
// the LLM context (TTL is 7 days here — enough to bridge any reasonable
// "they replied to your message from yesterday" gap).
//
// Redis schema:
//   tgmsg:{sessionId}:{messageId} → JSON { role, text, ts }
//   - sessionId matches the InboundMessage sessionId shape
//     ("telegram:<chatId>" or "telegram:<chatId>:<threadId>")
//   - messageId is the Telegram numeric message_id
//   - role is "assistant" (we sent it) or "user" (we received it)

import { getStore } from "@/app/lib/store";

export type TgMappedMessage = {
  role: "assistant" | "user";
  text: string;
  ts: number;
};

const TTL_SECONDS = 7 * 24 * 60 * 60;

function key(sessionId: string, messageId: number): string {
  return `tgmsg:${sessionId}:${messageId}`;
}

export async function saveTgMessage(args: {
  sessionId: string;
  messageId: number;
  role: "assistant" | "user";
  text: string;
}): Promise<void> {
  if (!Number.isFinite(args.messageId) || args.messageId <= 0) return;
  const store = getStore();
  await store.set(
    key(args.sessionId, args.messageId),
    {
      role: args.role,
      // Cap stored text — we only need enough to anchor the agent's
      // context. 4000 chars is well above the chat model's working window.
      text: (args.text ?? "").slice(0, 4000),
      ts: Date.now(),
    },
    { exSeconds: TTL_SECONDS }
  );
}

export async function loadTgMessage(
  sessionId: string,
  messageId: number
): Promise<TgMappedMessage | null> {
  if (!Number.isFinite(messageId) || messageId <= 0) return null;
  const store = getStore();
  return await store.get<TgMappedMessage>(key(sessionId, messageId));
}
