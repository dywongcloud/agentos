// app/steps/compileWorkforceStep.ts
//
// Natural-language → structured Workforce compiler. The user types
// `/team create <free-form description>`; this step asks an LLM to emit the
// team's trigger (same flat fields as the automation compiler), its member
// agents (name/emoji/persona/toolkits), and the ordered stages. The /team
// handler then resolves agent names against the tenant's existing agents
// (creating new ones as needed), persists the Workforce, and registers the
// trigger as a normal Automation with action { mode: "workforce", workforceId }.
//
// Flat schema throughout — no z.record, no discriminated unions — because
// OpenAI strict structured output rejects propertyNames and is far more
// reliable with nullable per-kind fields. Model: `meta` (gpt-5.4), `meta-pro`
// on retry — never gpt-5.2.

import { generateObject } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import {
  narrowTrigger,
  resolveComposioTriggerSlug,
  type CompiledTrigger,
} from "@/app/steps/compileAutomationStep";

export type CompiledWorkforceAgent = {
  name: string;
  emoji: string;
  persona: string;
  toolkits: string[];
};

export type CompiledWorkforceStage =
  | { kind: "agents"; agentNames: string[] }
  | { kind: "route"; instruction: string; candidateNames: string[]; maxPick?: number };

export type CompiledWorkforce = {
  name: string;
  emoji?: string;
  summary: string;
  trigger: CompiledTrigger;
  agents: CompiledWorkforceAgent[];
  stages: CompiledWorkforceStage[];
};

const agentSchema = z.object({
  name: z.string(),
  emoji: z.string(),
  persona: z.string(),
  toolkits: z.array(z.string()),
});

const stageSchema = z.object({
  kind: z.enum(["agents", "route"]),
  // agents stage
  agentNames: z.array(z.string()).nullable(),
  // route stage
  routeInstruction: z.string().nullable(),
  candidateNames: z.array(z.string()).nullable(),
  maxPick: z.number().nullable(),
});

const compileWorkforceSchema = z.object({
  name: z.string(),
  emoji: z.string().nullable(),
  summary: z.string(),

  triggerKind: z.enum(["schedule", "composio", "webhook", "chat"]),
  cron: z.string().nullable(),
  everyMs: z.number().nullable(),
  tz: z.string().nullable(),
  // composio trigger slug is resolved dynamically from the live catalog; the
  // model supplies a toolkit + event query, plus an optional best-guess slug.
  composioToolkit: z.string().nullable(),
  composioQuery: z.string().nullable(),
  composioTriggerType: z.string().nullable(),
  composioFilter: z.string().nullable(),
  chatPattern: z.string().nullable(),
  chatFlags: z.string().nullable(),

  agents: z.array(agentSchema),
  stages: z.array(stageSchema),
});

function buildSystem(args: { existingAgents: Array<{ name: string; toolkits: string[] }> }): string {
  const existing = args.existingAgents.length
    ? args.existingAgents
        .map((a) => `  - "${a.name}" (toolkits: ${a.toolkits.join(", ") || "none"})`)
        .join("\n")
    : "  (none yet)";
  return [
    "You compile a user's natural-language description of an agent TEAM",
    "(workforce) into a strict structured spec. A workforce is: a trigger, a",
    "set of named specialist sub-agents, and an ordered list of stages that run",
    "like a workflow when the trigger fires. Agents within a stage run in",
    "PARALLEL; each stage sees the text outputs of all earlier stages.",
    "",
    "TRIGGER — pick exactly ONE triggerKind:",
    "  schedule — time-based. `cron` (5-field) for calendar cadences, OR",
    "             `everyMs` for fixed intervals; `tz` = IANA zone or null (UTC).",
    "  composio — external app event (ANY connected app). Do NOT pick from a",
    "             fixed list: set `composioToolkit` to the app's toolkit slug and",
    "             `composioQuery` to a short phrase for the event ('new email',",
    "             'pull request opened', 'row added'). The exact trigger slug is",
    "             resolved from the live catalog (incl. polling triggers for apps",
    "             with no native events, e.g. monday.com). Optionally put a known",
    "             slug in `composioTriggerType` as a fallback. `composioFilter` is",
    "             a JSON object STRING of payload substrings (or null).",
    "  webhook  — fires when an external system POSTs to a minted URL. ALSO the",
    "             default when the user describes no trigger at all (the team",
    "             can still be run manually with /team run).",
    "  chat     — fires when an inbound chat message matches `chatPattern`",
    "             (JS regex source, no slashes; `chatFlags` default 'i').",
    "Set every trigger field you are not using to null.",
    "",
    "AGENTS — define each specialist the team needs:",
    "  name     — short, memorable, unique within the team (e.g. 'Scout').",
    "  emoji    — a single emoji avatar.",
    "  persona  — 1-3 sentences describing the agent's specialty and how it",
    "             should behave; written as a system-prompt addition.",
    "  toolkits — lowercase Composio toolkit slugs the agent may use, e.g.",
    "             gmail, googlesheets, googledocs, googledrive, googlecalendar,",
    "             github, slack, notion, linear, exa, firecrawl. Use [] for a",
    "             pure-reasoning agent. Scope tightly: only what the role needs.",
    "The user's EXISTING agents (reuse one by giving its exact name instead of",
    "redefining it — only list it in `agents` if you want to reuse it as-is):",
    existing,
    "",
    "STAGES — the workflow order. Each stage is either:",
    "  kind 'agents' — `agentNames` lists which agents act (in parallel, max 4).",
    "  kind 'route'  — an AI router picks who acts: `routeInstruction` says how",
    "                  to choose, `candidateNames` lists the options, `maxPick`",
    "                  caps how many are picked (default 1).",
    "Set the unused fields of each stage to null. Keep it simple: 1-3 stages is",
    "typical (e.g. research stage → write stage; or a single route stage).",
    "Every name in agentNames/candidateNames MUST appear in `agents` or in the",
    "existing-agents list above.",
    "",
    "Also set: `name` (short team name), `emoji` (team avatar or null), and a",
    "one-sentence `summary` of what the team does when triggered.",
  ].join("\n");
}

export async function compileWorkforceStep(args: {
  spec: string;
  existingAgents: Array<{ name: string; toolkits: string[] }>;
  retry?: boolean;
}): Promise<CompiledWorkforce> {
  "use step";

  const llm = buildLlmArgs({
    purpose: args.retry ? "meta-pro" : "meta",
    temperature: 0.2,
  });

  const result = await generateObject({
    model: (llm as any).model,
    ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
    ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
    schema: compileWorkforceSchema,
    system: buildSystem({ existingAgents: args.existingAgents }),
    prompt: `Compile this workforce/team request:\n\n${args.spec}`,
  });

  const o = result.object;

  // Resolve the exact Composio trigger slug dynamically (native + custom
  // polling types) instead of a hardcoded list — same path as automations.
  if (o.triggerKind === "composio") {
    const resolved = await resolveComposioTriggerSlug({
      toolkit: o.composioToolkit,
      query: o.composioQuery,
      guess: o.composioTriggerType,
    });
    o.composioTriggerType = resolved.slug;
  }

  const agents: CompiledWorkforceAgent[] = (o.agents ?? []).map((a) => ({
    name: a.name.trim(),
    emoji: a.emoji.trim() || "🤖",
    persona: a.persona.trim(),
    toolkits: (a.toolkits ?? []).map((t) => t.trim().toLowerCase()).filter(Boolean),
  }));

  const knownNames = new Set([
    ...agents.map((a) => a.name.toLowerCase()),
    ...args.existingAgents.map((a) => a.name.toLowerCase()),
  ]);

  const stages: CompiledWorkforceStage[] = [];
  for (const s of o.stages ?? []) {
    if (s.kind === "route") {
      const candidates = (s.candidateNames ?? []).filter((n) =>
        knownNames.has(n.trim().toLowerCase())
      );
      if (!candidates.length) continue;
      stages.push({
        kind: "route",
        instruction: s.routeInstruction?.trim() || "Pick the best agent for this event.",
        candidateNames: candidates,
        ...(s.maxPick && s.maxPick > 1 ? { maxPick: Math.min(4, s.maxPick) } : {}),
      });
    } else {
      const names = (s.agentNames ?? []).filter((n) => knownNames.has(n.trim().toLowerCase()));
      if (!names.length) continue;
      stages.push({ kind: "agents", agentNames: names.slice(0, 4) });
    }
  }
  // Degenerate compile (no usable stages): run all defined agents in one stage.
  if (!stages.length && agents.length) {
    stages.push({ kind: "agents", agentNames: agents.map((a) => a.name).slice(0, 4) });
  }
  if (!stages.length) {
    throw new Error("compileWorkforceStep produced no agents or stages");
  }

  return {
    name: o.name?.trim() || "Untitled team",
    ...(o.emoji?.trim() ? { emoji: o.emoji.trim() } : {}),
    summary: o.summary?.trim() || o.name?.trim() || "Workforce",
    trigger: narrowTrigger(o),
    agents,
    stages,
  };
}
