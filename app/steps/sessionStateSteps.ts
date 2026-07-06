// app/steps/sessionStateSteps.ts
import type { ModelMessage } from "ai";
import { loadTgMessage, saveTgMessage } from "@/app/lib/tgMessageMap";
import { getStore } from "@/app/lib/store";
import { captureChatOutcome, stashTurnAttribution } from "@/app/lib/learn/outcomeSignal";
import type { ChatRouteDecision } from "@/app/lib/learn/routerBias";

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

  // History is stored as a Redis LIST (lpush + ltrim), index 0 = newest.
  // lrange(key, 0, -1) returns all entries newest-first; reverse for
  // chronological order expected by the session workflow (oldest first).
  //
  // Migration: before the LIST format was introduced, history was stored as a
  // JSON-array string (GET/SET). If lrange returns a WRONGTYPE error (the key
  // exists as a STRING), fall back to store.get() to read the old format and
  // return it. On the next saveHistoryStep call the key will be deleted and
  // rewritten as a LIST, completing the one-time per-session migration.
  const store = getStore();
  let raw: string[] = [];
  try {
    raw = await store.lrange(historyKey(sessionId), 0, -1);
  } catch (err: any) {
    if (String(err?.message ?? "").includes("WRONGTYPE")) {
      const old = await store.get<ModelMessage[]>(historyKey(sessionId)).catch(() => null);
      return Array.isArray(old) ? old : [];
    }
    return [];
  }
  if (raw.length === 0) return [];
  return raw
    .map((s) => {
      try {
        return JSON.parse(s) as ModelMessage;
      } catch {
        return null;
      }
    })
    .filter((m): m is ModelMessage => m !== null)
    .reverse();
}

// Both learn-subsystem calls perform a non-idempotent Redis write (a
// stability-weighted stat update / a mailbox overwrite). sessionWorkflow is
// "use workflow" (not itself a step) — WDK's crash-retry model re-executes
// any NON-step code in the workflow body on a mid-run retry, which would
// double-apply these writes. Wrapping both in one "use step" call makes them
// checkpoint together: on a caller-level retry, this step either already
// completed (cached, skipped) or re-runs cleanly from scratch — never both.
export async function captureChatOutcomeStep(args: {
  tenantId: string;
  sessionId: string;
  newUserText: string;
  priorAssistantText: string;
  chatRoute: ChatRouteDecision;
}): Promise<void> {
  "use step";
  try {
    await captureChatOutcome(args.tenantId, args.sessionId, args.newUserText, args.priorAssistantText);
  } catch {
    // never break the turn
  }
  if (args.chatRoute.bucket) {
    try {
      await stashTurnAttribution(args.tenantId, args.sessionId, {
        chat: { arm: args.chatRoute.arm, bucket: args.chatRoute.bucket },
      });
    } catch {
      // never break the turn
    }
  }
}

export async function saveHistoryStep(sessionId: string, history: ModelMessage[]) {
  "use step";

  // Rewrite the list atomically: delete the old key and lpush all entries
  // in reverse order (oldest first in the push loop → index 0 ends up as
  // newest after all pushes, consistent with lpush + ltrim convention).
  const store = getStore();
  const key = historyKey(sessionId);
  const max = Number(process.env.HISTORY_MAX_MESSAGES ?? "30");
  const trimmed = history.length > max ? history.slice(history.length - max) : history;
  await store.del(key);
  // Push oldest-first so after all lpushes index 0 is the newest entry.
  for (const msg of trimmed) {
    await store.lpush(key, JSON.stringify(msg));
  }
}
