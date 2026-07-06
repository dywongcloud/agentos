// app/ui/workflows/workforceFlows.ts
//
// RUNTIME converter: turns a stored Workforce (team of scoped sub-agents) +
// its trigger automation rule into the same presentation-only `Flow` model the
// NBRI diagrams use, so each workforce renders as a real flow diagram on the
// /ui/workflows dashboard WITHOUT a rebuild or manifest regeneration. The team,
// its stages, and each agent's persona/toolkits are read live from Redis at
// request time, so newly-created teams appear immediately.
//
// Mapping (workforce → Flow vocabulary the dashboard legend already explains):
//   trigger rule           → trigger node (schedule/composio/webhook/chat)
//   single-agent stage      → step node (the agent's unique task, from persona)
//   multi-agent stage       → parallel box (one lane per agent)
//   route stage             → decision node (one branch per candidate agent)
//   final delivery          → end node (summary → the team's chat channel)

import type { Automation, AutomationTrigger } from "@/app/lib/automations";
import type { SubAgent, Workforce, WorkforceStage } from "@/app/lib/agents";
import type { Flow, FlowNode } from "./nbriFlows";

function triggerSummary(t: AutomationTrigger | null): string {
  if (!t) return "manual / on demand";
  switch (t.kind) {
    case "schedule":
      return t.cron
        ? `schedule · cron ${t.cron}${t.tz ? ` (${t.tz})` : ""}`
        : `schedule · every ${Math.round((t.everyMs ?? 0) / 1000)}s`;
    case "composio":
      return `app event · ${t.triggerType}`;
    case "webhook":
      return "webhook · external POST";
    case "chat":
      return `chat message · /${t.pattern}/${t.flags ?? "i"}`;
  }
}

function triggerLabel(t: AutomationTrigger | null): string {
  if (!t) return "Manual trigger";
  switch (t.kind) {
    case "schedule":
      return t.cron ? `On schedule (${t.cron})` : "On interval";
    case "composio":
      return `When ${t.triggerType} fires`;
    case "webhook":
      return "When webhook is called";
    case "chat":
      return `When chat matches /${t.pattern}/`;
  }
}

// The agent's persona IS its unique, prompt-derived task. Trim it to a tidy
// one/two-sentence task statement for the node detail.
function agentTask(persona: string): string {
  const clean = (persona || "").replace(/\s+/g, " ").trim();
  if (!clean) return "general assistant";
  // First two sentences, capped — enough to convey the distinct job.
  const sentences = clean.split(/(?<=[.!?])\s+/).slice(0, 2).join(" ");
  const out = sentences || clean;
  return out.length > 180 ? out.slice(0, 177).trimEnd() + "…" : out;
}

function toolkitsActor(a: SubAgent): string | undefined {
  if (!a.toolkits.length) return undefined;
  return a.toolkits.map((t) => t.toLowerCase()).join(" · ");
}

function agentLabel(a: SubAgent): string {
  const name = a.name?.trim() || a.id;
  return `${a.emoji ? `${a.emoji} ` : ""}${name}`;
}

function stageToNodes(
  stage: WorkforceStage,
  index: number,
  byId: Map<string, SubAgent>
): FlowNode[] {
  if (stage.kind === "route") {
    const candidates = stage.candidateAgentIds
      .map((id) => byId.get(id))
      .filter((a): a is SubAgent => !!a);
    if (!candidates.length) return [];
    return [
      {
        kind: "decision",
        label: stage.instruction?.trim()
          ? `Route: ${stage.instruction.trim()}`
          : `Route to ${stage.maxPick ?? 1} of ${candidates.length}`,
        branches: candidates.map((a) => ({
          label: agentLabel(a),
          nodes: [
            {
              kind: "step",
              label: agentLabel(a),
              detail: agentTask(a.persona),
              actor: toolkitsActor(a),
              auto: "draft",
            },
          ],
        })),
      },
    ];
  }

  const agents = stage.agentIds
    .map((id) => byId.get(id))
    .filter((a): a is SubAgent => !!a);
  if (!agents.length) return [];

  if (agents.length === 1) {
    const a = agents[0];
    return [
      {
        kind: "step",
        label: agentLabel(a),
        detail: agentTask(a.persona),
        actor: toolkitsActor(a),
        auto: "auto",
      },
    ];
  }

  return [
    {
      kind: "parallel",
      label: `Stage ${index + 1} — runs in parallel`,
      lanes: agents.map((a) => ({
        label: agentLabel(a),
        detail: agentTask(a.persona),
        actor: toolkitsActor(a),
        auto: "auto",
      })),
    },
  ];
}

export function workforceToFlow(
  team: Workforce,
  agents: SubAgent[],
  rule: Automation | null
): Flow {
  const byId = new Map(agents.map((a) => [a.id, a] as const));
  const trigger = rule?.trigger ?? null;

  const nodes: FlowNode[] = [
    {
      kind: "trigger",
      label: triggerLabel(trigger),
      detail: team.spec?.trim() ? team.spec.trim().slice(0, 160) : undefined,
      actor: team.channel,
    },
  ];

  team.stages.forEach((stage, i) => {
    nodes.push(...stageToNodes(stage, i, byId));
  });

  nodes.push({
    kind: "end",
    label: `Compose summary → ${team.channel}`,
  });

  const memberCount = new Set(
    team.stages.flatMap((s) =>
      s.kind === "route" ? s.candidateAgentIds : s.agentIds
    )
  ).size;

  return {
    id: team.id,
    title: `${team.emoji ? `${team.emoji} ` : ""}${team.name}`,
    phase: `${team.stages.length} stage${team.stages.length === 1 ? "" : "s"} · ${memberCount} agent${memberCount === 1 ? "" : "s"}${team.enabled ? "" : " · paused"}`,
    triggerSummary: triggerSummary(trigger),
    nodes,
  };
}
