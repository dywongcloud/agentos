// app/lib/messageReaction.ts
//
// Decide whether (and how) the bot should react to a user's Telegram message
// with an emoji — the little ❤️/😂/🔥 that pops on a message bubble. The goal
// is to feel like a real person texting back: react when there's a genuine
// social or emotional hook, stay quiet on dry logistics. Reacting to EVERY
// message feels robotic, so the default leans toward "none".
//
// Cheap + non-blocking by design:
//   - Runs on the cheap fast-meta model (NOT the flagship chat model) — this
//     is a background delight classifier that fires on every conversational
//     message, so paying flagship per-token here just to pick one emoji is
//     pure waste. The strong chat model is reserved for the actual reply.
//   - Tiny prompt, single-token-ish answer.
//   - The caller fires this via waitUntil so it never delays the actual
//     reply. The reaction landing a beat after the message is itself human.
//   - Every failure path returns null (no reaction); it must never break a
//     conversation.

import { generateText } from "ai";
import { z } from "zod/v4";
import { generateObject } from "ai";

import { env } from "@/app/lib/env";
import { resolveModel, resolveModelName } from "@/app/lib/modelRouting";
import { TELEGRAM_ALLOWED_REACTIONS } from "@/app/lib/providers/telegram";

// A curated, friendly subset of Telegram's allowed reactions. We bias the
// model toward these so reactions feel natural rather than reaching for the
// weird long-tail (🗿, 🍌). The model may still pick any allowed emoji; we
// validate against the full set before sending.
const SUGGESTED = [
  "❤", "🔥", "😁", "🤣", "👍", "🙏", "👀", "🤔", "🤯", "🎉",
  "🥰", "👏", "💯", "😭", "😎", "🤝", "👌", "😍", "🫡", "🤗",
];

const schema = z.object({
  react: z
    .boolean()
    .describe("Whether to react at all. Default false unless there's a real hook."),
  emoji: z
    .string()
    .nullable()
    .describe("Single emoji to react with when react=true, else null."),
});

function reactionsEnabled(): boolean {
  return (env("TELEGRAM_REACTIONS_ENABLED") ?? "true") !== "false";
}

// Fast pre-filter: skip the LLM entirely for messages that obviously don't
// warrant a reaction (commands, empty, very long pastes). Keeps cost near
// zero for the common non-conversational cases.
function shouldSkipFast(text: string): boolean {
  const t = (text ?? "").trim();
  if (!t) return true;
  if (t.startsWith("/")) return true; // slash commands get text acks, not reactions
  if (t.length > 1200) return true; // big pastes/logs — reacting is odd
  return false;
}

export async function pickReactionEmoji(text: string): Promise<string | null> {
  if (!reactionsEnabled() || shouldSkipFast(text)) return null;

  const modelName = resolveModelName("fast-meta");
  const system = [
    "You add emoji reactions to a friend's text messages, like on iMessage or",
    "Telegram. React the way a warm, witty friend would — SELECTIVELY.",
    "",
    "React when the message is funny, exciting, sweet, impressive, sad,",
    "grateful, surprising, or otherwise carries feeling. Skip dry logistics,",
    "neutral questions, and plain task requests — those get a real reply, not",
    "a reaction. When unsure, don't react.",
    "",
    "Aim to react to maybe a third of messages, not all of them. One emoji.",
    `Prefer these: ${SUGGESTED.join(" ")}`,
  ].join("\n");

  try {
    const { object } = await generateObject({
      model: resolveModel(modelName),
      schema,
      system,
      prompt: `Message from the user:\n"""${text.slice(0, 600)}"""`,
      temperature: 0.4,
    });
    if (!object.react || !object.emoji) return null;
    const emoji = object.emoji.trim();
    // Validate against Telegram's allowed set; reject anything else.
    if ((TELEGRAM_ALLOWED_REACTIONS as readonly string[]).includes(emoji)) {
      return emoji;
    }
    // Common normalization: strip variation selector (❤️ → ❤) which Telegram
    // expects without it for some emoji.
    const stripped = emoji.replace(/️/g, "");
    if ((TELEGRAM_ALLOWED_REACTIONS as readonly string[]).includes(stripped)) {
      return stripped;
    }
    return null;
  } catch {
    // generateObject can be flaky on some providers for tiny schemas — fall
    // back to a plain-text single-emoji ask before giving up.
    try {
      const { text: out } = await generateText({
        model: resolveModel(modelName),
        temperature: 0.4,
        system:
          "Reply with exactly ONE emoji to react to this message like a friend " +
          "would, or the word NONE if no reaction fits. Nothing else. Prefer: " +
          SUGGESTED.join(" "),
        prompt: text.slice(0, 600),
      });
      const cand = out.trim().replace(/️/g, "");
      if (cand && cand.toUpperCase() !== "NONE") {
        for (const e of TELEGRAM_ALLOWED_REACTIONS) {
          if (cand.includes(e)) return e;
        }
      }
    } catch {
      // give up silently
    }
    return null;
  }
}
