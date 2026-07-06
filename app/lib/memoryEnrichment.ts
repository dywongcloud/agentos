// app/lib/memoryEnrichment.ts
//
// A cheap LLM turns raw user-supplied or agent-observed text into a structured
// memory entry: title, summary, labels, importance, kind classification,
// type-specific fields (e.g. parsed path for "directory", parsed command +
// args for "command").
//
// This is the ONLY LLM in the memory pipeline. Reads use plain Redis ops
// (see memoryStore.ts), so the agent's hot path doesn't pay for it.
//
// Tunable: MEMORY_ENRICHMENT_MODEL env (default "gpt-4o-mini"). This is a
// background labeling/classification call (title, summary, labels, kind) that
// fires on every memory save — gpt-4o-mini is plenty for structured tagging,
// and ~16x cheaper than gpt-4o. Bump to gpt-4o / gpt-5.4-mini via env if
// classification quality ever regresses.

import { generateObject } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";
import { z } from "zod/v4";

import { env } from "@/app/lib/env";
import {
  MEMORY_KINDS,
  type MemoryKind,
  type PutMemoryInput,
} from "@/app/lib/memoryStore";

function enrichmentModelName(): string {
  return env("MEMORY_ENRICHMENT_MODEL") ?? "gpt-4o-mini";
}

const KIND_ENUM = z.enum(MEMORY_KINDS as unknown as [MemoryKind, ...MemoryKind[]]);

const enrichmentSchema = z.object({
  kind: KIND_ENUM,
  title: z
    .string()
    .min(1)
    .max(140),
  summary: z.string().min(1).max(500),
  labels: z.array(z.string().min(1).max(48)).min(0).max(10),
  importance: z.number().min(0).max(1),
  // Type-specific structured shapes — the model fills the keys that apply
  // for the chosen kind, leaves others null.
  fields: z.object({
    path: z.string().nullable(),
    command: z.string().nullable(),
    args: z.array(z.string()).nullable(),
    when_to_use: z.string().nullable(),
    related: z.array(z.string()).nullable(),
  }),
});

export type EnrichmentInput = {
  // Raw text — what the user said OR what the agent observed.
  content: string;
  // Optional hint from the caller about the kind (e.g. when the agent calls
  // `remember` with an explicit kind, we still want the LLM to fill labels +
  // summary but we lock the kind in).
  kindHint?: MemoryKind;
  // Optional context — recent chat / surrounding info — to help the model
  // classify and label accurately. We don't store this; it just informs.
  context?: string;
};

export type EnrichedMemory = Pick<
  PutMemoryInput,
  "kind" | "title" | "summary" | "labels" | "importance" | "fields"
>;

export async function enrichMemory(
  input: EnrichmentInput
): Promise<EnrichedMemory> {
  const model = enrichmentModelName();

  const system = [
    "You convert a raw note into a structured agent-memory entry.",
    "",
    "Pick the best `kind` from the enum. Definitions:",
    "  directory      — a filesystem path the user works in (/workspace/x, ~/notes)",
    "  command        — a shell command or pattern the user runs (npm run dev, etc.)",
    "  preference     — a stable user preference (prefers pnpm, uses TS strict)",
    "  fact           — an assertion about the user / their stack / project",
    "  code_snippet   — a small reusable code fragment",
    "  workflow       — a multi-step process the user follows",
    "  person         — name + context for a collaborator / contact",
    "  project        — a named project + metadata",
    "  credential_hint — pointer to where a credential lives (NEVER the secret itself)",
    "  chat_summary   — a distilled summary of a past conversation / session",
    "  favorite_app   — a Composio toolkit/integration the user clearly favors",
    "  other          — anything else worth remembering",
    "",
    "Output:",
    "  title       — short noun phrase, what this is",
    "  summary     — one paragraph, natural language",
    "  labels      — 1-6 lowercase kebab-case tags useful for retrieval",
    "  importance  — 0..1, how reusable/load-bearing across future conversations",
    "  fields      — for directory/command kinds, populate path / command / args;",
    "                otherwise leave the relevant entries null.",
    "",
    "Never copy a secret value into title/summary/fields. If the input looks",
    "like a credential, set kind=credential_hint and describe only WHERE it",
    "is, not WHAT it is.",
  ].join("\n");

  const userPrompt = [
    input.kindHint
      ? `Caller hinted kind=${input.kindHint}. Use it unless the content clearly says otherwise.`
      : "",
    input.context ? `Context (do not store, just for classification):\n${input.context}\n` : "",
    `Raw note:\n${input.content}`,
  ]
    .filter(Boolean)
    .join("\n");

  try {
    const result = await generateObject({
      model: textAuxModel(model),
      schema: enrichmentSchema,
      system,
      prompt: userPrompt,
      temperature: 0.2,
    });
    const o = result.object;

    // Compact the fields object — drop nulls so we don't store junk keys.
    const fields: Record<string, unknown> = {};
    if (o.fields.path) fields.path = o.fields.path;
    if (o.fields.command) fields.command = o.fields.command;
    if (o.fields.args && o.fields.args.length) fields.args = o.fields.args;
    if (o.fields.when_to_use) fields.when_to_use = o.fields.when_to_use;
    if (o.fields.related && o.fields.related.length)
      fields.related = o.fields.related;

    return {
      kind: input.kindHint ?? o.kind,
      title: o.title,
      summary: o.summary,
      labels: o.labels,
      importance: o.importance,
      fields: Object.keys(fields).length ? fields : undefined,
    };
  } catch (err) {
    // Enrichment failure is non-fatal — store a minimal entry with raw
    // content so the user's data isn't lost.
    return {
      kind: input.kindHint ?? "other",
      title: input.content.slice(0, 60).trim() || "memory",
      summary: undefined,
      labels: [],
      importance: 0.3,
      fields: undefined,
    };
  }
}
