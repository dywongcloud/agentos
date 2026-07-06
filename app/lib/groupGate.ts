// app/lib/groupGate.ts
//
// "Should the bot reply to this group message?" gate for team (workspace)
// group chats. In a shared group, people mostly talk to EACH OTHER — the bot
// replying to every message makes it unusable. Policy:
//
//   1. ADDRESSED → always respond (deterministic, zero tokens):
//        - Telegram reply-gesture on one of the bot's own messages
//        - @botusername mention anywhere in the text
//        - the bot's display name used as a word ("suri, pull up the sheet")
//   2. Otherwise → a cheap fast-meta LLM gate decides whether the bot has
//      something genuinely valuable to chime in on (an implicit question it
//      can answer, a factual correction, an action it can take). Strict
//      default-NO so normal human-to-human chatter stays bot-free.
//   3. Not responding still RECORDS the message into the session history
//      (speaker-labeled) so when the bot IS addressed later it has the full
//      conversation context.
//
// Scoped by the caller to team-bound groups only; DMs are unaffected.
// TEAM_CHIME_IN=0 disables the LLM gate (bot only replies when addressed).

import { generateObject } from "ai";
import { z } from "zod/v4";

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import { buildLlmArgs } from "@/app/lib/modelRouting";
import { telegramGetMe } from "@/app/lib/providers/telegram";
import { loadTgMessage } from "@/app/lib/tgMessageMap";
import type { InboundMessage } from "@/app/lib/normalize";

// --- addressed check (deterministic) -----------------------------------------

// Word-boundary match so the name "Ana" doesn't fire on "banana".
function containsWord(text: string, word: string): boolean {
  if (!word || word.length < 2) return false;
  const esc = word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|\\W)${esc}(\\W|$)`, "i").test(text);
}

export async function isAddressedToBot(msg: InboundMessage): Promise<boolean> {
  const text = msg.text ?? "";

  // Explicit reply-gesture on one of the bot's own messages.
  if (msg.replyToTgMessageId) {
    try {
      const bound = await loadTgMessage(msg.sessionId, msg.replyToTgMessageId);
      if (bound?.role === "assistant") return true;
    } catch {
      // side-table miss — fall through to the text checks
    }
  }

  try {
    const me = await telegramGetMe();
    if (me.username && text.toLowerCase().includes(`@${me.username.toLowerCase()}`)) {
      return true;
    }
    if (me.firstName && containsWord(text, me.firstName)) return true;
  } catch {
    // getMe unavailable — err on the side of NOT addressed; the reply-gesture
    // and slash-command paths still work, and the chime gate can still fire.
  }
  return false;
}

// --- silent history recording -------------------------------------------------

const historyKey = (sessionId: string) => `sess:${sessionId}:history`;

// Record a group message the bot chose not to answer, speaker-labeled, so the
// conversation context is intact when the bot is addressed later. Mirrors the
// session workflow's history shape (ModelMessage[]) and trim cap.
export async function recordSilentGroupMessage(msg: InboundMessage): Promise<void> {
  try {
    const store = getStore();
    const history =
      ((await store.get<Array<{ role: string; content: unknown }>>(
        historyKey(msg.sessionId)
      )) as Array<{ role: string; content: unknown }>) ?? [];
    const who = msg.senderUsername ? `@${msg.senderUsername}` : `user ${msg.senderId}`;
    history.push({
      role: "user",
      content: `[${who}, not addressed to you — you stayed silent]: ${msg.text ?? ""}`,
    });
    const max = Number(process.env.HISTORY_MAX_MESSAGES ?? "30");
    const trimmed =
      history.length > max ? history.slice(history.length - max) : history;
    await store.set(historyKey(msg.sessionId), trimmed);
  } catch {
    // best-effort — losing one silent message is better than failing ingress
  }
}

// --- chime-in gate (cheap LLM) -------------------------------------------------

const chimeSchema = z.object({
  chime: z.boolean(),
  reason: z.string(),
});

// Flatten recent history into a compact transcript for the gate prompt.
async function recentTranscript(sessionId: string, take = 8): Promise<string> {
  try {
    const history =
      ((await getStore().get<Array<{ role: string; content: unknown }>>(
        historyKey(sessionId)
      )) as Array<{ role: string; content: unknown }>) ?? [];
    return history
      .slice(-take)
      .map((m) => {
        const c = m.content;
        const t =
          typeof c === "string"
            ? c
            : Array.isArray(c)
              ? (c as Array<{ text?: string }>).map((p) => p?.text ?? "").join(" ")
              : "";
        return `${m.role === "assistant" ? "BOT" : "USER"}: ${t.slice(0, 300)}`;
      })
      .filter((l) => l.length > 6)
      .join("\n");
  } catch {
    return "";
  }
}

// Deterministic pre-gate (zero tokens): the LLM chime decision only makes sense
// for a message that plausibly asks for something the bot could serve. Pure
// human banter/acks — the overwhelming majority of group traffic — skip the
// model entirely. Conservative: anything with a question or a request-shaped
// verb passes through to the LLM; only clearly-inert chatter is dropped here
// (exactly the cases the LLM would rate chime=false anyway).
const CHIME_SIGNAL =
  /\?|\b(can|could|would|should|how|what|whats|when|who|where|why|which|anyone|someone|somebody|is there|are there|need|help|find|search|look\s?up|remind|schedule|send|e-?mail|draft|pull|fetch|get|make|create|build|summari[sz]e|update|check|link|remember|list|show|track|automate)\b/i;
const CHIME_BANTER =
  /^(?:(?:hi|hey+|hello|yo|sup|lol|lmao|haha+|hehe+|ok(?:ay)?|kk?|thanks?|thank you|ty|np|nice|cool|great|gotcha|yep|yeah|ya|nah|nope|no|yes|sure|same|agreed|true|fr|bet|word|facts|👍|🙏|😂|🔥|❤️?|😅|🙌)[\s!.,]*)+$/i;

function passesChimePrefilter(text: string): boolean {
  const t = text.trim();
  if (t.length < 8) return false; // too short to be a real request
  if (CHIME_BANTER.test(t)) return false; // pure acknowledgement/banter
  return CHIME_SIGNAL.test(t); // require a request-shaped signal
}

export async function shouldChimeIn(msg: InboundMessage): Promise<boolean> {
  if ((env("TEAM_CHIME_IN") ?? "1") === "0") return false;
  if (!passesChimePrefilter(msg.text ?? "")) return false;
  try {
    const transcript = await recentTranscript(msg.sessionId);
    const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0 });
    const { object } = await generateObject({
      model: llm.model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: chimeSchema,
      system: [
        "You gate an assistant bot in a team group chat where humans mostly talk",
        "to each other. Given the latest message (NOT addressed to the bot),",
        "decide if the bot should chime in anyway.",
        "",
        "chime=true ONLY when the bot can add clear, concrete value right now:",
        "  - the message asks something the bot can directly answer or do",
        "    (even though it wasn't named), e.g. 'does anyone remember the",
        "    spreadsheet link?' when the bot created that spreadsheet",
        "  - someone states a fact the bot knows to be materially wrong and it",
        "    matters to the team's work",
        "  - the team is trying to do something the bot can do instantly",
        "chime=false for EVERYTHING else: greetings, banter, opinions,",
        "scheduling between humans, anything ambiguous, anything where a human",
        "is clearly the intended responder. When unsure, chime=false.",
      ].join("\n"),
      prompt:
        (transcript ? `Recent conversation:\n${transcript}\n\n` : "") +
        `Latest message (from ${msg.senderUsername ? "@" + msg.senderUsername : "a member"}):\n${(msg.text ?? "").slice(0, 800)}`,
    });
    return object.chime === true;
  } catch {
    // Gate failure must never spam the group — stay silent.
    return false;
  }
}
