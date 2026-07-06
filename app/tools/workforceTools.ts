// app/tools/workforceTools.ts
//
// Conversational workforce management for the MAIN chat agent (sub-agents
// get the edit-only subset via `editOnly` — they shouldn't spawn teams).
// Lets "make a workforce where Scout and others run daily at 8am" work
// without the user ever typing /team:
//
//   list_agents_and_teams — roster of sub-agents + workforces with triggers
//   create_team_agent     — define one new scoped sub-agent
//   create_workforce      — NL spec → compiled team + registered trigger
//   update_workforce      — patch mission/name/emoji/trigger-filter in place
//   run_workforce         — fire a team's pipeline right now
//   set_workforce_enabled — pause/resume a team's trigger

import { tool, type ToolSet } from "ai";
import { z } from "zod/v4";

import type { Channel } from "@/app/lib/identity";
import { createWorkforceFromSpec } from "@/app/lib/workforceService";
import {
  putSubAgent,
  listAgentsByTenant,
  listWorkforcesByTenant,
  listAgentBotsByTenant,
  getWorkforce,
  putWorkforce,
} from "@/app/lib/agents";
import {
  getAutomation,
  putAutomation,
  fireAutomation,
  setEnabled as setAutomationEnabled,
} from "@/app/lib/automations";

export type WorkforceToolsContext = {
  tenantId: string;
  channel: Channel;
  sessionId: string;
  baseUrl: string;
};

function describeTrigger(rule: Awaited<ReturnType<typeof getAutomation>>): string {
  if (!rule) return "manual only";
  const t = rule.trigger;
  switch (t.kind) {
    case "schedule":
      return t.cron
        ? `cron ${t.cron}${t.tz ? ` (${t.tz})` : ""}`
        : `every ${Math.round((t.everyMs ?? 0) / 1000)}s`;
    case "composio":
      return `on ${t.triggerType}`;
    case "webhook":
      return "webhook";
    case "chat":
      return `chat /${t.pattern}/${t.flags ?? "i"}`;
  }
}

// `editOnly` is the sub-agent surface: a scoped team member may inspect the
// roster and correct its own team's mission/filter when the user asks, but
// must not spawn, fire, or pause teams from inside a team run / bot chat.
export function makeWorkforceTools(
  ctx: WorkforceToolsContext,
  opts?: { editOnly?: boolean }
): ToolSet {
  const listAgentsAndTeams = tool({
    description: [
      "List this user's sub-agents (scoped specialist agents) and workforces",
      "(teams of agents that run like a workflow when a trigger fires).",
      "Call this FIRST whenever the user mentions a team, workforce, or an",
      "agent by name, so you reference real ids and avoid duplicates.",
    ].join("\n"),
    inputSchema: z.object({}),
    execute: async () => {
      const [agents, teams, bots] = await Promise.all([
        listAgentsByTenant(ctx.tenantId),
        listWorkforcesByTenant(ctx.tenantId),
        listAgentBotsByTenant(ctx.tenantId),
      ]);
      const botByAgent = new Map(bots.map((b) => [b.agentId, b.username] as const));
      const teamsOut = [];
      for (const t of teams) {
        const rule = t.automationId ? await getAutomation(t.automationId) : null;
        teamsOut.push({
          id: t.id,
          name: t.name,
          emoji: t.emoji ?? null,
          enabled: t.enabled,
          trigger: describeTrigger(rule),
          stages: t.stages.map((s) =>
            s.kind === "route"
              ? { kind: "route", candidates: s.candidateAgentIds }
              : { kind: "agents", agentIds: s.agentIds }
          ),
        });
      }
      return {
        ok: true,
        agents: agents.map((a) => ({
          id: a.id,
          name: a.name,
          emoji: a.emoji,
          toolkits: a.toolkits,
          telegramBot: botByAgent.get(a.id) ?? null,
        })),
        teams: teamsOut,
      };
    },
  });

  const createTeamAgent = tool({
    description: [
      "Create ONE new scoped sub-agent (a named specialist limited to specific",
      "Composio toolkits). Use when the user asks for a single new agent.",
      "For a whole team/workforce, prefer create_workforce — it can define",
      "multiple agents at once. Toolkits are lowercase Composio slugs like",
      "gmail, googlesheets, github, slack, notion, linear, exa, firecrawl;",
      "use [] for a pure-reasoning agent. To give the agent its own Telegram",
      "bot the user must run: /agent bind <agentId> <botToken from @BotFather>.",
    ].join("\n"),
    inputSchema: z.object({
      name: z.string().min(1).max(60),
      persona: z.string().min(1).max(600),
      toolkits: z.array(z.string()),
      emoji: z.string().nullable(),
    }),
    execute: async (input) => {
      const existing = await listAgentsByTenant(ctx.tenantId);
      const clash = existing.find(
        (a) => a.name.toLowerCase() === input.name.trim().toLowerCase()
      );
      if (clash) {
        return {
          ok: false,
          error: `An agent named "${clash.name}" already exists (${clash.id}).`,
        };
      }
      const a = await putSubAgent({
        tenantId: ctx.tenantId,
        name: input.name.trim(),
        emoji: input.emoji?.trim() || "🤖",
        persona: input.persona.trim(),
        toolkits: input.toolkits,
      });
      return {
        ok: true,
        agentId: a.id,
        name: a.name,
        toolkits: a.toolkits,
        bindHint: `/agent bind ${a.id} <botToken>`,
      };
    },
  });

  const createWorkforce = tool({
    description: [
      "Create a WORKFORCE: a team of sub-agents that runs like a workflow",
      "whenever its trigger fires (schedule, app event, webhook, or chat",
      "pattern). Pass ONE free-form description covering: the trigger, each",
      "member agent's role (reuse existing agents by their exact name —",
      "check list_agents_and_teams first), and the stage order. Stages run",
      "sequentially; agents within a stage run in parallel and each stage",
      "sees earlier stages' outputs. Example description: 'every weekday at",
      "8am EST, Scout triages my gmail inbox, then a Writer agent drafts",
      "replies and a Notifier posts a summary'. The compiled team is",
      "persisted and its trigger goes live immediately — confirm the plan",
      "with the user before calling if anything is ambiguous.",
    ].join("\n"),
    inputSchema: z.object({
      description: z.string().min(10).max(4000),
    }),
    execute: async (input) => {
      const { team, rule, note, newAgents, members } = await createWorkforceFromSpec({
        tenantId: ctx.tenantId,
        channel: ctx.channel,
        sessionId: ctx.sessionId,
        spec: input.description,
        baseUrl: ctx.baseUrl,
      });
      return {
        ok: true,
        teamId: team.id,
        name: team.name,
        emoji: team.emoji ?? null,
        trigger: describeTrigger(rule),
        triggerNote: note,
        stages: team.stages.map((s, i) =>
          s.kind === "route"
            ? `stage ${i + 1}: AI routes among ${s.candidateAgentIds.length} agents`
            : `stage ${i + 1}: ${s.agentIds
                .map((id) => members.find((m) => m.id === id)?.name ?? id)
                .join(" + ")} (parallel)`
        ),
        newAgents: newAgents.map((a) => ({ id: a.id, name: a.name, toolkits: a.toolkits })),
        manage: `/team run ${team.id} · /team pause ${team.id} · /team delete ${team.id}`,
      };
    },
  });

  const runWorkforce = tool({
    description:
      "Fire a workforce's pipeline RIGHT NOW (manual run). The composed summary is delivered to this chat when all stages finish. Get the teamId from list_agents_and_teams.",
    inputSchema: z.object({
      teamId: z.string().min(1),
    }),
    execute: async (input) => {
      const team = await getWorkforce(input.teamId.trim());
      if (!team || team.tenantId !== ctx.tenantId) {
        return { ok: false, error: `No team with id ${input.teamId}` };
      }
      const runId = await fireAutomation(team.automationId, "manual", {
        manual: true,
        ts: Date.now(),
      });
      return runId
        ? { ok: true, runId, team: team.name }
        : { ok: false, error: "couldn't start the run (is the team's trigger rule enabled?)" };
    },
  });

  const updateWorkforce = tool({
    description: [
      "Update an existing workforce IN PLACE: its mission text (the TEAM",
      "MISSION every member agent reads on each run), display name, emoji,",
      "and/or its app-event trigger filter. Use this for corrections — e.g.",
      "a wrong email address in the mission — instead of deleting and",
      "recreating the team. Pass null for any field you are not changing.",
      "When the mission mentions a specific sender/value that the trigger",
      "also filters on, update BOTH mission and triggerFilterJson so they",
      "stay consistent. triggerFilterJson is a JSON object of substring",
      'matches against the event payload, e.g. {"from":"someone@gmail.com"}',
      "(composio/app-event triggers only). Changing the trigger KIND",
      "(schedule ↔ app event ↔ webhook ↔ chat) is not supported — delete and",
      "recreate the team for that. Get the teamId from list_agents_and_teams.",
    ].join("\n"),
    inputSchema: z.object({
      teamId: z.string().min(1),
      mission: z.string().nullable(),
      name: z.string().nullable(),
      emoji: z.string().nullable(),
      triggerFilterJson: z.string().nullable(),
    }),
    execute: async (input) => {
      const team = await getWorkforce(input.teamId.trim());
      if (!team || team.tenantId !== ctx.tenantId) {
        return { ok: false, error: `No team with id ${input.teamId}` };
      }

      const mission = input.mission?.trim() || null;
      const name = input.name?.trim() || null;
      const emoji = input.emoji?.trim() || null;

      let filter: Record<string, string> | null = null;
      if (input.triggerFilterJson?.trim()) {
        try {
          const parsed: unknown = JSON.parse(input.triggerFilterJson);
          if (
            !parsed ||
            typeof parsed !== "object" ||
            Array.isArray(parsed) ||
            Object.values(parsed).some((v) => typeof v !== "string")
          ) {
            throw new Error("not a flat string map");
          }
          filter = parsed as Record<string, string>;
        } catch {
          return {
            ok: false,
            error:
              'triggerFilterJson must be a JSON object of string values, e.g. {"from":"someone@gmail.com"}',
          };
        }
      }

      const rule = team.automationId ? await getAutomation(team.automationId) : null;
      if (filter && rule?.trigger.kind !== "composio") {
        return {
          ok: false,
          error: `trigger filter only applies to app-event triggers (this team's trigger is ${rule?.trigger.kind ?? "missing"})`,
        };
      }

      const updated = await putWorkforce({
        ...team,
        ...(mission ? { spec: mission } : {}),
        ...(name ? { name } : {}),
        ...(emoji ? { emoji } : {}),
      });

      const changed: string[] = [];
      if (mission) changed.push("mission");
      if (name) changed.push("name");
      if (emoji) changed.push("emoji");

      // Keep the trigger rule's copy in sync so the dashboard + run prompts
      // never show a stale spec.
      if (rule) {
        await putAutomation({
          ...rule,
          ...(name ? { name } : {}),
          ...(mission ? { spec: mission } : {}),
          ...(filter && rule.trigger.kind === "composio"
            ? { trigger: { ...rule.trigger, filter } }
            : {}),
        });
        if (filter) changed.push("trigger filter");
      }

      if (!changed.length) {
        return { ok: false, error: "nothing to update — all fields were null/empty" };
      }
      return {
        ok: true,
        team: updated.name,
        teamId: updated.id,
        updated: changed,
        mission: updated.spec,
      };
    },
  });

  const setWorkforceEnabled = tool({
    description:
      "Pause (enabled=false) or resume (enabled=true) a workforce's trigger. Get the teamId from list_agents_and_teams.",
    inputSchema: z.object({
      teamId: z.string().min(1),
      enabled: z.boolean(),
    }),
    execute: async (input) => {
      const team = await getWorkforce(input.teamId.trim());
      if (!team || team.tenantId !== ctx.tenantId) {
        return { ok: false, error: `No team with id ${input.teamId}` };
      }
      await setAutomationEnabled(team.automationId, input.enabled);
      await putWorkforce({ ...team, enabled: input.enabled });
      return { ok: true, team: team.name, enabled: input.enabled };
    },
  });

  if (opts?.editOnly) {
    return {
      list_agents_and_teams: listAgentsAndTeams,
      update_workforce: updateWorkforce,
    };
  }
  return {
    list_agents_and_teams: listAgentsAndTeams,
    create_team_agent: createTeamAgent,
    create_workforce: createWorkforce,
    update_workforce: updateWorkforce,
    run_workforce: runWorkforce,
    set_workforce_enabled: setWorkforceEnabled,
  };
}
