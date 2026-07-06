// app/lib/workforceManifest.ts
//
// Builds a WDK manifest fragment from the live workforces in Redis, so the
// dashboard's patched fetchWorkflowsManifest can merge user-created teams into
// the manifest object at request time (no rebuild, no enrich pass). Mirrors the
// shape scripts/nbriWorkflows.mjs produces for the static NBRI flows.
//
// Returned shape (merged into manifest.workflows):
//   { "app/workflows/workforces.ts": { <name>: { workflowId, graph } } }

import {
  listAllWorkforces,
  listWorkforcesByTenant,
  getAgentsByIds,
  type Workforce,
} from "@/app/lib/agents";
import { getAutomation } from "@/app/lib/automations";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import { workforceToFlow } from "@/app/ui/workflows/workforceFlows";
import { flowToGraph } from "@/app/ui/workflows/flowToGraph";
import {
  HC_TEAM_ID,
  hardcodedSubAgents,
  hardcodedWorkforce,
  hardcodedTrigger,
} from "@/app/lib/hardcodedWorkforce";
import type { Automation } from "@/app/lib/automations";

const FILE_KEY = "app/workflows/workforces.ts";

// Make a manifest-safe, unique workflow identifier from a team name + id.
function safeName(name: string, id: string): string {
  const base = (name || "workforce")
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 40) || "workforce";
  return `${base}__${id}`;
}

export async function buildWorkforceManifestFragment(): Promise<{
  workflows: Record<string, Record<string, { workflowId: string; graph: unknown }>>;
}> {
  // Union the cross-tenant index with the owner tenant's teams. The global
  // index only catches teams created/run after it was introduced, so the owner
  // union ensures pre-existing teams still surface immediately.
  const owner = await resolveUiTenant(null);
  const [all, ownerTeams] = await Promise.all([
    listAllWorkforces(),
    owner ? listWorkforcesByTenant(owner) : Promise.resolve([] as Workforce[]),
  ]);
  const byId = new Map<string, Workforce>();
  for (const t of [...all, ...ownerTeams]) byId.set(t.id, t);
  const teams = [...byId.values()].sort((a, b) => a.createdAt - b.createdAt);
  const out: Record<string, { workflowId: string; graph: unknown }> = {};

  for (const team of teams) {
    const memberIds = Array.from(
      new Set(
        team.stages.flatMap((s) =>
          s.kind === "route" ? s.candidateAgentIds : s.agentIds
        )
      )
    );
    const [agents, rule] = await Promise.all([
      getAgentsByIds(memberIds),
      team.automationId ? getAutomation(team.automationId) : Promise.resolve(null),
    ]);
    const flow = workforceToFlow(team, agents, rule);
    const graph = flowToGraph(flow);
    const name = safeName(team.name, team.id);
    out[name] = {
      workflowId: `workflow//./app/workflows/workforces//${name}`,
      graph,
    };
  }

  // Hardcoded showcase workforce (purely visual; never stored or run). Owner
  // tenant is irrelevant for the diagram, so pass a placeholder.
  try {
    const hcTeam = hardcodedWorkforce("admin");
    const hcAgents = hardcodedSubAgents("admin");
    const hcRule = { trigger: hardcodedTrigger() } as unknown as Automation;
    const hcFlow = workforceToFlow(hcTeam, hcAgents, hcRule);
    if (hcFlow.nodes[0]) hcFlow.nodes[0].label = "New chat message";
    const hcName = safeName(hcTeam.name, HC_TEAM_ID);
    out[hcName] = {
      workflowId: `workflow//./app/workflows/workforces//${hcName}`,
      graph: flowToGraph(hcFlow),
    };
  } catch {
    // showcase injection is best-effort; never block the real fragment
  }

  return { workflows: { [FILE_KEY]: out } };
}
