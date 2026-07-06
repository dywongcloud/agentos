// app/tools/summarizeChatTool.ts
//
// "summarize_chat_and_remember" — the agent reaches for this when the user
// says things like "remember our chat", "remember today's conversation", or
// when ending a long working session and wanting the bot to retain context.
//
// What it does:
//   1. Pulls the current session's chat history from Redis.
//   2. Asks gpt-4o to (a) write one concise paragraph-level summary of the
//      whole session, and (b) extract individual memorable facts /
//      preferences / commands / projects that should become their own
//      atomic memories.
//   3. Persists the paragraph summary as a `chat_summary` memory.
//   4. Persists the atomic items via the regular enrichment path (each gets
//      its own labels + kind classification via gpt-4o).
//
// Why split paragraph vs atomics: paragraph gives the agent a one-shot
// "what we talked about" recall; atomic memories give targeted recall
// (preference matches "do you prefer X?", command matches "what's the start
// command?", etc.).
//
// The summarize call uses gpt-4o (overridable via MEMORY_ENRICHMENT_MODEL).
// The atomics go through enrichMemory which also calls gpt-4o per item —
// can be expensive on a long chat. We cap atomic count at 8.

import { tool, generateObject } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";
import { z } from "zod/v4";

import { env } from "@/app/lib/env";
import { loadHistoryStep } from "@/app/steps/sessionStateSteps";
import { putMemory, type MemoryKind } from "@/app/lib/memoryStore";
import { enrichMemory } from "@/app/lib/memoryEnrichment";

export type SummarizeChatToolContext = {
  tenantId: string;
  sessionId: string;
};

const MAX_ATOMIC_ITEMS = 8;
const MAX_MESSAGES_TO_SUMMARIZE = 60;

const summarySchema = z.object({
  title: z.string().min(1).max(140),
  summary: z
    .string()
    .min(1)
    .max(1200)
    .describe(
      "One paragraph distilling the conversation. Focus on decisions, conclusions, and concrete details — not narration of the back-and-forth."
    ),
  labels: z.array(z.string().min(1).max(48)).min(0).max(8),
  atomic_facts: z
    .array(
      z.object({
        text: z.string().min(2).max(500),
        kind_hint: z
          .enum([
            "directory",
            "command",
            "preference",
            "fact",
            "code_snippet",
            "workflow",
            "person",
            "project",
            "credential_hint",
            "favorite_app",
            "other",
          ])
          .nullable(),
      })
    )
    .min(0)
    .max(MAX_ATOMIC_ITEMS),
});

function formatHistoryForSummary(
  history: Array<{ role: string; content: unknown }>
): string {
  const slice = history.slice(-MAX_MESSAGES_TO_SUMMARIZE);
  return slice
    .map((m, i) => {
      const role = m.role.toUpperCase();
      const c = m.content;
      let text = "";
      if (typeof c === "string") text = c;
      else if (Array.isArray(c))
        text = (c as Array<{ text?: string }>)
          .map((p) => p?.text ?? "")
          .filter(Boolean)
          .join(" ");
      return `${i + 1}. [${role}] ${text.slice(0, 800)}`;
    })
    .join("\n");
}

export function makeSummarizeChatTool(ctx: SummarizeChatToolContext) {
  return tool({
    description: [
      "Distill the current conversation into long-term memory. Use when:",
      "  - The user says 'remember our chat', 'remember today', 'save this conversation'",
      "  - You're wrapping up a long working session worth retaining",
      "  - The user wants a summary of what was decided",
      "",
      "What it does:",
      "  - Writes ONE chat_summary memory (paragraph-level recap)",
      "  - Extracts UP TO 8 atomic memorable items (preferences, commands,",
      "    directories, projects, etc.) and stores each as its own memory",
      "  - All entries are scoped to this tenant and retrievable later via recall()",
      "",
      "Use the optional `topic` argument when the user wants the summary",
      "focused on a particular thread of the conversation rather than the",
      "whole thing.",
    ].join("\n"),
    inputSchema: z.object({
      topic: z
        .string()
        .nullable()
        .describe(
          "Optional focus topic. When set, the summary emphasizes this aspect of the chat."
        ),
      max_messages: z
        .number()
        .int()
        .min(5)
        .max(200)
        .nullable()
        .describe(
          "How far back to read. Default 60. Increase only when explicitly asked to summarize 'all of our chat'."
        ),
    }),
    execute: async (args) => {
      const history = (await loadHistoryStep(ctx.sessionId)) as Array<{
        role: string;
        content: unknown;
      }>;
      if (!Array.isArray(history) || history.length === 0) {
        return { ok: false, error: "no chat history to summarize" };
      }

      const sliceLimit = args.max_messages ?? MAX_MESSAGES_TO_SUMMARIZE;
      const recent = history.slice(-sliceLimit);
      const formatted = formatHistoryForSummary(recent);

      // Use the dedicated CHAT_SUMMARY_MODEL knob (default gpt-4.1-mini —
      // ~10x cheaper than gpt-4o and accurate enough for distillation).
      // Falls back to the broader memory-enrichment knob, then a hard
      // default, so older deploys keep working.
      const model =
        env("CHAT_SUMMARY_MODEL") ??
        env("MEMORY_ENRICHMENT_MODEL") ??
        "gpt-4.1-mini";

      const system = [
        "You distill a recent agent/user chat into long-term memory.",
        "",
        "Output:",
        "  title          — short noun phrase for the summary memory",
        "  summary        — single paragraph; focus on decisions, conclusions,",
        "                   concrete details, NOT a transcript",
        "  labels         — 1-6 lowercase kebab-case tags for retrieval",
        "  atomic_facts   — 0-8 items worth remembering on their own. For",
        "                   each, include short text and a kind hint:",
        "                     command/directory/preference/project/etc.",
        "                   Skip transient details. Don't duplicate the",
        "                   summary — only standalone-useful items.",
        "",
        "Never copy secrets or tokens into any output. If credentials came up,",
        "describe ONLY where they live (kind_hint='credential_hint').",
      ].join("\n");

      const userPrompt = [
        args.topic ? `Focus topic: ${args.topic}\n` : "",
        `Recent conversation (${recent.length} messages):`,
        formatted,
      ]
        .filter(Boolean)
        .join("\n");

      let result;
      try {
        result = await generateObject({
          model: textAuxModel(model),
          schema: summarySchema,
          system,
          prompt: userPrompt,
          temperature: 0.3,
        });
      } catch (err: any) {
        return {
          ok: false,
          error: `summary failed: ${err?.message ?? String(err)}`,
        };
      }
      const o = result.object;

      // Persist the paragraph summary as a chat_summary memory.
      const summaryEntry = await putMemory({
        tenantId: ctx.tenantId,
        kind: "chat_summary",
        title: o.title,
        content: o.summary,
        summary: o.summary,
        labels: o.labels,
        importance: 0.7,
        fields: {
          messages_considered: recent.length,
          topic: args.topic ?? null,
          source: "summarize_chat_tool",
        },
      });

      // Persist atomic facts as individual memories. Each item runs through
      // the regular enrichMemory pipeline so labels + kind get a final-pass
      // classification.
      const atomicResults = await Promise.allSettled(
        o.atomic_facts.map(async (item) => {
          const enriched = await enrichMemory({
            content: item.text,
            kindHint: item.kind_hint ?? undefined,
            context: args.topic ? `topic: ${args.topic}` : undefined,
          });
          const entry = await putMemory({
            tenantId: ctx.tenantId,
            kind: enriched.kind,
            title: enriched.title,
            content: item.text,
            summary: enriched.summary,
            labels: enriched.labels,
            importance: enriched.importance,
            fields: enriched.fields,
          });
          return { id: entry.id, title: entry.title, kind: entry.kind };
        })
      );
      const storedAtomics = atomicResults
        .filter(
          (r): r is PromiseFulfilledResult<{ id: string; title: string; kind: MemoryKind }> =>
            r.status === "fulfilled"
        )
        .map((r) => r.value);

      return {
        ok: true,
        chat_summary: {
          id: summaryEntry.id,
          title: summaryEntry.title,
          labels: summaryEntry.labels,
        },
        atomic_memories: storedAtomics,
      };
    },
  });
}
