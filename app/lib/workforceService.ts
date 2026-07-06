// app/lib/workforceService.ts
//
// Shared "create a workforce from a natural-language spec" pipeline:
// compile (LLM) → resolve member names against the tenant's existing agents
// (creating new SubAgents as needed) → persist the Workforce → register its
// trigger as an Automation with action { mode: "workforce", workforceId } →
// patch the team with the rule id. Used by the /team create command handler
// and the chat agent's create_workforce tool, so plain conversation can build
// teams without slash commands.

import type { Channel } from "@/app/lib/identity";
import type { CompiledTrigger } from "@/app/steps/compileAutomationStep";
import { compileWorkforceStep } from "@/app/steps/compileWorkforceStep";
import { registerAutomation } from "@/app/lib/registerAutomation";
import { recordActivity } from "@/app/lib/activityLog";
import type { Automation } from "@/app/lib/automations";
import {
  putSubAgent,
  putWorkforce,
  listAgentsByTenant,
  type SubAgent,
  type Workforce,
  type WorkforceStage,
} from "@/app/lib/agents";

export type CreatedWorkforce = {
  team: Workforce;
  rule: Automation;
  note: string;
  newAgents: SubAgent[];
  members: SubAgent[];
};

export async function createWorkforceFromSpec(args: {
  tenantId: string;
  channel: Channel;
  sessionId: string;
  spec: string;
  baseUrl: string;
  retry?: boolean;
  // When the caller already knows the exact trigger (e.g. the /ui/agents
  // builder's picker), use it instead of whatever the compiler inferred.
  triggerOverride?: CompiledTrigger;
  triggerConfig?: Record<string, unknown>;
}): Promise<CreatedWorkforce> {
  const existing = await listAgentsByTenant(args.tenantId);
  const compiled = await compileWorkforceStep({
    spec: args.spec,
    existingAgents: existing.map((a) => ({ name: a.name, toolkits: a.toolkits })),
    ...(args.retry ? { retry: true } : {}),
  });

  // Resolve compiled agent names → existing agents (case-insensitive) or
  // newly created SubAgents.
  const byName = new Map(existing.map((a) => [a.name.toLowerCase(), a] as const));
  const newAgents: SubAgent[] = [];
  for (const ca of compiled.agents) {
    if (byName.has(ca.name.toLowerCase())) continue;
    const a = await putSubAgent({
      tenantId: args.tenantId,
      name: ca.name,
      emoji: ca.emoji,
      persona: ca.persona,
      toolkits: ca.toolkits,
    });
    byName.set(a.name.toLowerCase(), a);
    newAgents.push(a);
  }

  const idFor = (name: string) => byName.get(name.trim().toLowerCase())?.id;
  const stages: WorkforceStage[] = [];
  for (const s of compiled.stages) {
    if (s.kind === "route") {
      const candidateAgentIds = s.candidateNames
        .map(idFor)
        .filter((x): x is string => Boolean(x));
      if (candidateAgentIds.length) {
        stages.push({
          kind: "route",
          instruction: s.instruction,
          candidateAgentIds,
          ...(s.maxPick ? { maxPick: s.maxPick } : {}),
        });
      }
    } else {
      const agentIds = s.agentNames.map(idFor).filter((x): x is string => Boolean(x));
      if (agentIds.length) stages.push({ kind: "agents", agentIds });
    }
  }
  if (!stages.length) throw new Error("no usable stages after resolving agent names");

  let team = await putWorkforce({
    tenantId: args.tenantId,
    channel: args.channel,
    sessionId: args.sessionId,
    name: compiled.name,
    emoji: compiled.emoji,
    spec: args.spec,
    stages,
    automationId: "", // patched right after the trigger rule is registered
    enabled: true,
  });

  const { rule, note } = await registerAutomation({
    tenantId: args.tenantId,
    channel: args.channel,
    sessionId: args.sessionId,
    spec: args.spec,
    compiled: {
      name: compiled.name,
      summary: compiled.summary,
      trigger: args.triggerOverride ?? compiled.trigger,
      action: { mode: "workforce", workforceId: team.id },
    },
    baseUrl: args.baseUrl,
    ...(args.triggerConfig ? { triggerConfig: args.triggerConfig } : {}),
  });
  team = await putWorkforce({ ...team, automationId: rule.id });

  await recordActivity(args.tenantId, {
    kind: "automation",
    summary: `created team: ${team.name} (${stages.length} stages)`,
    meta: { workforceId: team.id, automationId: rule.id },
  });

  const memberIds = new Set<string>();
  for (const s of stages) {
    if (s.kind === "route") s.candidateAgentIds.forEach((id) => memberIds.add(id));
    else s.agentIds.forEach((id) => memberIds.add(id));
  }
  const members = [...byName.values()].filter((a) => memberIds.has(a.id));

  return { team, rule, note, newAgents, members };
}
