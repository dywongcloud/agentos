// app/ui/agents/page.tsx
//
// Agents & workforces surface — a relay.app-style ReactFlow canvas showing
// each team as trigger → stage columns → agent nodes (toolkit logo chips),
// with a detail side panel and live run highlighting. Server component loads
// the initial snapshot; WorkforceCanvas polls /api/ui/agents for updates.

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import AppShell from "@/app/ui/shell/AppShell";
import { workspaceLabel } from "@/app/ui/shell/tabs";
import {
  listAgentsByTenant,
  listWorkforcesByTenant,
  listAgentBotsByTenant,
} from "@/app/lib/agents";
import { getAutomation, type AutomationTrigger } from "@/app/lib/automations";
import {
  WECHAT_LOGO,
  hardcodedSubAgents,
  hardcodedWorkforce,
  hardcodedChatLog,
} from "@/app/lib/hardcodedWorkforce";

import WorkforceCanvas, {
  type CanvasAgent,
  type CanvasTeam,
  type CanvasBot,
} from "@/app/ui/agents/WorkforceCanvas";

export const dynamic = "force-dynamic";

type Sp = { userId?: string; embed?: string };

function triggerLabel(t: AutomationTrigger | null): string {
  if (!t) return "Manual";
  switch (t.kind) {
    case "schedule":
      return t.cron
        ? `Cron ${t.cron}${t.tz ? ` (${t.tz})` : ""}`
        : `Every ${Math.round((t.everyMs ?? 0) / 1000)}s`;
    case "composio":
      return t.triggerType.replace(/_/g, " ").toLowerCase();
    case "webhook":
      return "Webhook";
    case "chat":
      return `Chat /${t.pattern}/`;
  }
}

export default async function AgentsPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/agents", sp));
  const tenant = (await resolveUiTenant(sp.userId)) ?? "admin";
  const embed = sp.embed === "1" || sp.embed === "true";

  const [agents, teams, bots] = await Promise.all([
    listAgentsByTenant(tenant),
    listWorkforcesByTenant(tenant),
    listAgentBotsByTenant(tenant),
  ]);

  const canvasTeams: CanvasTeam[] = await Promise.all(
    teams.map(async (t) => {
      const rule = t.automationId ? await getAutomation(t.automationId) : null;
      return {
        id: t.id,
        name: t.name,
        emoji: t.emoji ?? null,
        spec: t.spec,
        stages: t.stages,
        automationId: t.automationId,
        enabled: t.enabled,
        triggerKind: rule?.trigger.kind ?? null,
        triggerLabel: triggerLabel(rule?.trigger ?? null),
        status: rule?.status ?? null,
      };
    })
  );

  const canvasAgents: CanvasAgent[] = agents.map((a) => ({
    id: a.id,
    name: a.name,
    emoji: a.emoji,
    persona: a.persona,
    toolkits: a.toolkits,
    skills: a.skills ?? null,
    telegramBotId: a.telegramBotId ?? null,
    modelName: a.modelName ?? null,
  }));

  const canvasBots: CanvasBot[] = bots.map((b) => ({
    botId: b.botId,
    agentId: b.agentId,
    username: b.username,
  }));

  // Hardcoded showcase team (purely visual; not stored, never runs). Appended
  // so it always appears in the canvas, listing, and office view.
  const hcTeam = hardcodedWorkforce(tenant);
  const hcAgents = hardcodedSubAgents(tenant);
  canvasTeams.push({
    id: hcTeam.id,
    name: hcTeam.name,
    emoji: hcTeam.emoji ?? null,
    spec: hcTeam.spec,
    stages: hcTeam.stages,
    automationId: hcTeam.automationId,
    enabled: hcTeam.enabled,
    triggerKind: "chat",
    triggerLabel: "New chat message",
    triggerLogo: WECHAT_LOGO,
    chatLog: hardcodedChatLog(),
    status: "active",
  });
  for (const a of hcAgents) {
    if (canvasAgents.some((x) => x.id === a.id)) continue;
    canvasAgents.push({
      id: a.id,
      name: a.name,
      emoji: a.emoji,
      persona: a.persona,
      toolkits: a.toolkits,
      skills: a.skills ?? null,
      telegramBotId: a.telegramBotId ?? null,
      modelName: a.modelName ?? null,
    });
  }

  const canvas = (
    <WorkforceCanvas
      userId={tenant}
      agents={canvasAgents}
      teams={canvasTeams}
      bots={canvasBots}
    />
  );

  if (embed) {
    return (
      <main
        style={{
          fontFamily:
            "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
          background: "var(--card)",
          minHeight: "100vh",
          padding: 12,
        }}
      >
        <div style={{ maxWidth: 1280, margin: "0 auto" }}>{canvas}</div>
      </main>
    );
  }

  return (
    <AppShell
      active="agents"
      userId={tenant}
      workspaceName={workspaceLabel(tenant)}
      title="Agents & Workforces"
      subtitle={
        <>
          Scoped sub-agents and their trigger-driven teams. Create with{" "}
          <code>/agent create</code> and <code>/team create</code> in chat.
        </>
      }
    >
      {canvas}
    </AppShell>
  );
}
