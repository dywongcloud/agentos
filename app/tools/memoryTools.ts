// app/tools/memoryTools.ts
//
// Three tools the agent calls autonomously to manage long-term memory.
// Per-tenant; the tool factory takes the tenantId at construction.
//
// remember(text, kindHint?)  → enriches via gpt-4o, stores
// recall(query?, kind?)      → deterministic retrieval (no LLM)
// forget(id|tag|kind)        → deletes matching entries

import { tool } from "ai";
import { z } from "zod/v4";

import {
  putMemory,
  deleteMemory,
  listByKind,
  listByTag,
  MEMORY_KINDS,
  type MemoryKind,
} from "@/app/lib/memoryStore";
import { enrichMemory } from "@/app/lib/memoryEnrichment";
import {
  retrieveRelevantMemories,
  memoriesToPromptBlock,
} from "@/app/lib/memoryRetrieval";

export type MemoryToolContext = {
  tenantId: string;
};

const KIND_ENUM = z.enum(
  MEMORY_KINDS as unknown as [MemoryKind, ...MemoryKind[]]
);

export function makeRememberTool(ctx: MemoryToolContext) {
  return tool({
    description: [
      "Save a fact, preference, command, directory, snippet, workflow, or any",
      "other piece of information you should remember about this user for",
      "future conversations. Use proactively when:",
      "  - the user states a preference (e.g. 'I always use pnpm')",
      "  - the user mentions a directory or command they work with",
      "  - a multi-step workflow comes up that you'll see again",
      "  - the user asks you to remember something explicitly",
      "",
      "Don't store transient details like 'they just asked for X' or",
      "'today they were debugging Y'. Memory is for things reusable across",
      "future conversations.",
      "",
      "The system labels + summarizes the text automatically. You",
      "may pass a kind_hint when you're confident; otherwise omit it.",
    ].join("\n"),
    inputSchema: z.object({
      text: z
        .string()
        .min(2)
        .describe(
          "The thing to remember, in plain language. Include enough context that the memory makes sense out of conversation."
        ),
      kind_hint: KIND_ENUM.nullable().describe(
        "Optional kind classification. Leave null to let the labeler decide."
      ),
    }),
    execute: async (args) => {
      const enriched = await enrichMemory({
        content: args.text,
        kindHint: args.kind_hint ?? undefined,
      });
      const entry = await putMemory({
        tenantId: ctx.tenantId,
        kind: enriched.kind,
        title: enriched.title,
        content: args.text,
        summary: enriched.summary,
        labels: enriched.labels,
        importance: enriched.importance,
        fields: enriched.fields,
      });
      return {
        ok: true,
        id: entry.id,
        kind: entry.kind,
        title: entry.title,
        labels: entry.labels,
      };
    },
  });
}

export function makeRecallTool(ctx: MemoryToolContext) {
  return tool({
    description: [
      "Look up memories about this user. Deterministic Redis query — no LLM,",
      "no fuzzy guessing. Use this when:",
      "  - the user asks 'what did you remember about X?'",
      "  - you need to verify a stored fact / preference before answering",
      "  - you want to ground a response in the user's specific environment",
      "    (their directories, commands, projects, preferences)",
      "",
      "You don't usually need to call recall manually for every turn — the",
      "top-relevant memories are already injected into your system prompt.",
      "Call recall when you need MORE memories than the auto-injected set,",
      "or memories for a specific kind/tag.",
    ].join("\n"),
    inputSchema: z.object({
      query: z
        .string()
        .nullable()
        .describe(
          "Free-text query. Keywords from this are used to rank memories. Optional."
        ),
      kind: KIND_ENUM.nullable().describe(
        "Filter to a single kind (e.g. 'directory', 'command')."
      ),
      tag: z
        .string()
        .nullable()
        .describe("Filter to memories with this tag/label."),
      limit: z
        .number()
        .int()
        .min(1)
        .max(40)
        .nullable()
        .describe("How many entries to return. Default 8."),
    }),
    execute: async (args) => {
      const limit = args.limit ?? 8;
      let entries;
      if (args.tag) {
        entries = await listByTag(ctx.tenantId, args.tag, limit);
      } else if (args.kind) {
        entries = await listByKind(ctx.tenantId, args.kind, limit);
      } else {
        entries = await retrieveRelevantMemories(ctx.tenantId, {
          query: args.query ?? undefined,
          limit,
        });
      }
      return {
        ok: true,
        count: entries.length,
        entries: entries.map((e) => ({
          id: e.id,
          kind: e.kind,
          title: e.title,
          summary: e.summary,
          labels: e.labels,
          fields: e.fields,
        })),
      };
    },
  });
}

export function makeForgetTool(ctx: MemoryToolContext) {
  return tool({
    description: [
      "Delete one or more memories. Use when:",
      "  - the user explicitly says 'forget X' or 'don't remember Y'",
      "  - a stored fact has become outdated and they correct you",
      "",
      "Be specific. Prefer id when known. Tag/kind deletion removes the",
      "entire matching group — confirm with the user before doing that.",
    ].join("\n"),
    inputSchema: z.object({
      id: z.string().nullable().describe("Memory id to delete (preferred)."),
      tag: z
        .string()
        .nullable()
        .describe("Delete ALL memories matching this tag."),
      kind: KIND_ENUM.nullable().describe(
        "Delete ALL memories of this kind."
      ),
    }),
    execute: async (args) => {
      if (args.id) {
        const ok = await deleteMemory(ctx.tenantId, args.id);
        return { ok, deleted: ok ? 1 : 0 };
      }
      let entries;
      if (args.tag) entries = await listByTag(ctx.tenantId, args.tag, 999);
      else if (args.kind) entries = await listByKind(ctx.tenantId, args.kind, 999);
      else return { ok: false, deleted: 0, error: "Specify id, tag, or kind." };
      let n = 0;
      for (const e of entries) {
        if (await deleteMemory(ctx.tenantId, e.id)) n++;
      }
      return { ok: true, deleted: n };
    },
  });
}

// Re-export the prompt block helper here for convenience in agentTurn.
export { memoriesToPromptBlock };
