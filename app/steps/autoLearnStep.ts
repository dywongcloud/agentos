// app/steps/autoLearnStep.ts
//
// Passive "memory that grows with you" — distills durable facts/preferences out
// of ongoing chat WITHOUT the user having to type /remember. The user is
// cost-sensitive (this whole thread started with "every new email burns
// tokens"), so this is gated hard:
//
//   • Throttled to once per session per AUTO_LEARN_INTERVAL_HOURS (default 6h).
//   • Only runs when >= AUTO_LEARN_MIN_NEW new turns have accrued since the last
//     run (a cursor over the history length).
//   • ONE small gpt-4.1-mini call per run that returns already-enriched items —
//     no per-item enrichment fan-out — so cost is a single cheap call per
//     window, never per message.
//   • Disable entirely with AUTO_LEARN_CHAT=0.
//
// Net effect: bounded, predictable spend (≤ a few cents/day for an active
// chatter) for memory that keeps learning on its own.

import type { ModelMessage } from "ai";
import { generateObject } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";
import { z } from "zod/v4";

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";
import { putMemory, MEMORY_KINDS, type MemoryKind } from "@/app/lib/memoryStore";
import { recordActivity } from "@/app/lib/activityLog";

const lastKey = (sessionId: string) => `autolearn:last:${sessionId}`;
const cursorKey = (sessionId: string) => `autolearn:cursor:${sessionId}`;

function enabled(): boolean {
  return (env("AUTO_LEARN_CHAT") ?? "1") !== "0";
}
function intervalMs(): number {
  const h = Number(env("AUTO_LEARN_INTERVAL_HOURS") ?? "6");
  return (Number.isFinite(h) && h > 0 ? h : 6) * 60 * 60 * 1000;
}
function minNew(): number {
  const n = Number(env("AUTO_LEARN_MIN_NEW") ?? "8");
  return Number.isFinite(n) && n > 0 ? n : 8;
}

function flattenContent(c: unknown): string {
  if (typeof c === "string") return c;
  if (Array.isArray(c)) {
    return (c as Array<{ text?: string }>)
      .map((p) => p?.text ?? "")
      .filter(Boolean)
      .join(" ");
  }
  return "";
}

// Best-effort, throttled. Returns the count of memories learned (0 when it
// no-ops, which is the common case).
export async function maybeAutoLearnChatStep(args: {
  tenantId: string;
  sessionId: string;
  history: ModelMessage[];
}): Promise<{ learned: number }> {
  "use step";
  if (!enabled()) return { learned: 0 };

  const store = getStore();
  const history = Array.isArray(args.history) ? args.history : [];
  const len = history.length;

  try {
    // Cursor = history length at the last successful run. Need enough NEW turns.
    const cursor = Number((await store.get<number>(cursorKey(args.sessionId))) ?? 0);
    if (len - cursor < minNew()) return { learned: 0 };

    // Throttle: claim the window atomically so concurrent turns don't double-run.
    const claimed = await store.set(lastKey(args.sessionId), String(Date.now()), {
      exSeconds: Math.ceil(intervalMs() / 1000),
      nx: true,
    });
    if (!claimed) return { learned: 0 };

    // Distill the NEW slice (plus a little overlap for context).
    const slice = history.slice(Math.max(0, cursor - 2));
    const formatted = slice
      .map((m, i) => {
        const t = flattenContent((m as { content?: unknown }).content);
        const role = String((m as { role?: string }).role ?? "user").toUpperCase();
        return t ? `${i + 1}. [${role}] ${t.slice(0, 700)}` : "";
      })
      .filter(Boolean)
      .join("\n");

    if (!formatted.trim()) {
      await store.set(cursorKey(args.sessionId), len);
      return { learned: 0 };
    }

    const schema = z.object({
      items: z
        .array(
          z.object({
            kind: z.enum(MEMORY_KINDS as unknown as [MemoryKind, ...MemoryKind[]]),
            title: z.string().min(1).max(120),
            content: z.string().min(2).max(600),
            summary: z.string().min(1).max(400),
            labels: z.array(z.string().min(1).max(40)).max(6),
            importance: z.number().min(0).max(1),
          })
        )
        .max(8),
    });

    // Cheap distiller — NOT a chat/meta default route, so the gpt-5.2 ban
    // doesn't apply; gpt-4.1-mini is the right tool for cheap extraction.
    const model =
      env("CHAT_SUMMARY_MODEL") ?? env("MEMORY_ENRICHMENT_MODEL") ?? "gpt-4.1-mini";

    const { object } = await generateObject({
      model: textAuxModel(model),
      schema,
      system: [
        "You silently maintain a user's long-term memory from their chat.",
        "Extract ONLY durable, reusable facts worth remembering across future",
        "sessions: stable preferences, their stack/tools, recurring directories",
        "or commands, named projects/people, workflows they follow.",
        "Do NOT extract: one-off questions, transient task state, pleasantries,",
        "anything already obvious, or ANY secret/credential value.",
        "Return 0 items if nothing durable was said. Be conservative.",
        "Pick the most fitting `kind` for each item. labels are lowercase",
        "kebab-case. importance 0..1 = how load-bearing/reusable it is.",
      ].join("\n"),
      prompt: `Recent conversation (${slice.length} messages):\n${formatted}`,
      temperature: 0.2,
    });

    let learned = 0;
    for (const it of object.items) {
      try {
        await putMemory({
          tenantId: args.tenantId,
          kind: it.kind,
          title: it.title,
          content: it.content,
          summary: it.summary,
          labels: it.labels,
          importance: it.importance,
          fields: { source: "auto_learn_chat", sessionId: args.sessionId },
        });
        learned++;
      } catch {
        // skip a single bad item
      }
    }

    await store.set(cursorKey(args.sessionId), len);

    if (learned > 0) {
      await recordActivity(args.tenantId, {
        kind: "memory",
        summary: `auto-learned ${learned} memory item(s) from chat`,
        meta: { sessionId: args.sessionId, source: "auto_learn_chat" },
      }).catch(() => undefined);
    }

    return { learned };
  } catch {
    return { learned: 0 };
  }
}
