// app/lib/agents.ts
//
// Storage for sub-agents and workforces (teams).
//
// A SubAgent is a named persona scoped to a set of Composio toolkits (and
// optionally a subset of inline skills). A Workforce groups agents into an
// ordered list of stages that execute like a workflow run: agents within a
// stage run in parallel; each stage sees the outputs of the stages before it.
// A workforce's trigger is stored as a normal Automation rule whose action is
// { mode: "workforce", workforceId } — so all four trigger sources (schedule,
// Composio event, webhook, chat) reuse the automations plumbing unchanged and
// every firing produces a normal AutomationRun.
//
// Redis layout (no TTL — standing config + run stage records):
//
//   agent:{id}              JSON   SubAgent
//   agents:by_tenant:{t}    SET    agent ids
//   wfteam:{id}             JSON   Workforce
//   wfteams:by_tenant:{t}   SET    workforce ids
//   wfrun:{automationRunId} JSON   WorkforceStageRecord[] (appended per stage)
//   tgbot:{botId}           JSON   AgentTelegramBot
//   tgbots:by_tenant:{t}    SET    bot ids
//
// This module is workflow-reachable: no Node builtins, JSON-serializable types.

import { getStore } from "@/app/lib/store";
import type { Channel } from "@/app/lib/identity";

// --- types ----------------------------------------------------------------

export type SubAgent = {
  id: string;
  tenantId: string;
  name: string;
  emoji: string;
  persona: string; // system-prompt addition describing the agent's specialty
  toolkits: string[]; // composio toolkit slugs (lowercase, e.g. "gmail")
  skills?: string[]; // inline-skill allowlist; undefined = all skills
  telegramBotId?: string; // bound dedicated bot, if any
  modelName?: string; // per-agent LLM override (catalog id); undefined = default
  createdAt: number;
  updatedAt: number;
};

// The JSON-serializable scope handed to agentTurn / executeAgentTurnStep.
export type SubAgentScope = {
  agentId: string;
  name: string;
  emoji: string;
  persona: string;
  toolkits: string[];
  skills?: string[];
  modelName?: string; // per-agent LLM override; agentTurn honors it first
};

export type WorkforceStage =
  | { kind: "agents"; agentIds: string[] } // run in parallel
  | {
      kind: "route";
      // An LLM picks which candidate agent(s) should handle this event.
      instruction: string;
      candidateAgentIds: string[];
      maxPick?: number; // default 1
    };

export type Workforce = {
  id: string;
  tenantId: string;
  channel: Channel;
  sessionId: string;
  name: string;
  emoji?: string;
  spec: string; // raw natural-language description
  stages: WorkforceStage[];
  automationId: string; // the trigger rule (action.mode === "workforce")
  enabled: boolean;
  createdAt: number;
  lastRunId?: string;
};

export type AgentTelegramBot = {
  botId: string;
  tenantId: string;
  agentId: string;
  token: string; // BotFather token (plaintext at rest, accepted by the user)
  secret: string; // x-telegram-bot-api-secret-token for the per-bot webhook
  username: string; // @username from getMe
  createdAt: number;
};

export type WorkforceStageRecord = {
  stageIndex: number;
  kind: "agents" | "route";
  pickedAgentIds?: string[]; // for route stages
  outputs: Array<{
    agentId: string;
    agentName: string;
    jobId?: string;
    status: "ok" | "error";
    text: string;
  }>;
  ts: number;
};

// --- keys -------------------------------------------------------------------

const agentKey = (id: string) => `agent:${id}`;
const agentsByTenantKey = (t: string) => `agents:by_tenant:${t}`;
const teamKey = (id: string) => `wfteam:${id}`;
const teamsByTenantKey = (t: string) => `wfteams:by_tenant:${t}`;
// Global (cross-tenant) workforce index. The WDK dashboard's manifest is not
// tenant-scoped, so dynamic workforce-diagram injection enumerates every team.
const teamsAllKey = () => `wfteams:all`;
const wfRunKey = (runId: string) => `wfrun:${runId}`;
const botKey = (botId: string) => `tgbot:${botId}`;
const botsByTenantKey = (t: string) => `tgbots:by_tenant:${t}`;

function shortId(prefix: string): string {
  return prefix + "_" + globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 12);
}

// --- sub-agent CRUD ---------------------------------------------------------

export async function putSubAgent(
  a: Omit<SubAgent, "id" | "createdAt" | "updatedAt"> & {
    id?: string;
    createdAt?: number;
  }
): Promise<SubAgent> {
  const store = getStore();
  const now = Date.now();
  const full: SubAgent = {
    id: a.id ?? shortId("ag"),
    tenantId: a.tenantId,
    name: a.name,
    emoji: a.emoji,
    persona: a.persona,
    toolkits: (a.toolkits ?? []).map((t) => String(t).trim().toLowerCase()).filter(Boolean),
    skills: a.skills,
    telegramBotId: a.telegramBotId,
    modelName: a.modelName,
    createdAt: a.createdAt ?? now,
    updatedAt: now,
  };
  await store.set(agentKey(full.id), full);
  await store.sadd(agentsByTenantKey(full.tenantId), full.id);
  return full;
}

export async function getSubAgent(id: string): Promise<SubAgent | null> {
  return getStore().get<SubAgent>(agentKey(id));
}

export async function getAgentsByIds(ids: string[]): Promise<SubAgent[]> {
  const out: SubAgent[] = [];
  for (const id of ids) {
    const a = await getSubAgent(id);
    if (a) out.push(a);
  }
  return out;
}

export async function listAgentsByTenant(tenantId: string): Promise<SubAgent[]> {
  const store = getStore();
  const ids = await store.smembers(agentsByTenantKey(tenantId));
  const out = await getAgentsByIds(ids);
  out.sort((a, b) => a.createdAt - b.createdAt);
  return out;
}

export async function deleteSubAgent(id: string): Promise<void> {
  const store = getStore();
  const a = await getSubAgent(id);
  if (!a) return;
  await store.del(agentKey(id));
  await store.srem(agentsByTenantKey(a.tenantId), id);
}

export function scopeForAgent(a: SubAgent): SubAgentScope {
  return {
    agentId: a.id,
    name: a.name,
    emoji: a.emoji,
    persona: a.persona,
    toolkits: a.toolkits,
    skills: a.skills,
    modelName: a.modelName,
  };
}

// --- workforce CRUD -----------------------------------------------------------

export async function putWorkforce(
  w: Omit<Workforce, "id" | "createdAt"> & { id?: string; createdAt?: number }
): Promise<Workforce> {
  const store = getStore();
  const full: Workforce = {
    id: w.id ?? shortId("team"),
    tenantId: w.tenantId,
    channel: w.channel,
    sessionId: w.sessionId,
    name: w.name,
    emoji: w.emoji,
    spec: w.spec,
    stages: w.stages,
    automationId: w.automationId,
    enabled: w.enabled,
    createdAt: w.createdAt ?? Date.now(),
    lastRunId: w.lastRunId,
  };
  await store.set(teamKey(full.id), full);
  await store.sadd(teamsByTenantKey(full.tenantId), full.id);
  await store.sadd(teamsAllKey(), full.id);
  return full;
}

export async function getWorkforce(id: string): Promise<Workforce | null> {
  return getStore().get<Workforce>(teamKey(id));
}

export async function listWorkforcesByTenant(tenantId: string): Promise<Workforce[]> {
  const store = getStore();
  const ids = await store.smembers(teamsByTenantKey(tenantId));
  const out: Workforce[] = [];
  for (const id of ids) {
    const w = await getWorkforce(id);
    if (w) out.push(w);
  }
  out.sort((a, b) => a.createdAt - b.createdAt);
  return out;
}

export async function deleteWorkforce(id: string): Promise<void> {
  const store = getStore();
  const w = await getWorkforce(id);
  if (!w) return;
  await store.del(teamKey(id));
  await store.srem(teamsByTenantKey(w.tenantId), id);
  await store.srem(teamsAllKey(), id);
}

// Enumerate every workforce across all tenants (for the dashboard manifest).
export async function listAllWorkforces(): Promise<Workforce[]> {
  const store = getStore();
  const ids = await store.smembers(teamsAllKey());
  const out: Workforce[] = [];
  for (const id of ids) {
    const w = await getWorkforce(id);
    if (w) out.push(w);
  }
  out.sort((a, b) => a.createdAt - b.createdAt);
  return out;
}

// --- workforce run stage records ---------------------------------------------

export async function getStageRecords(runId: string): Promise<WorkforceStageRecord[]> {
  return (await getStore().get<WorkforceStageRecord[]>(wfRunKey(runId))) ?? [];
}

export async function appendStageRecord(
  runId: string,
  record: WorkforceStageRecord
): Promise<void> {
  const store = getStore();
  const records = await getStageRecords(runId);
  records.push(record);
  await store.set(wfRunKey(runId), records);
}

// --- per-agent telegram bots ---------------------------------------------------

export async function putAgentBot(
  b: Omit<AgentTelegramBot, "createdAt"> & { createdAt?: number }
): Promise<AgentTelegramBot> {
  const store = getStore();
  const full: AgentTelegramBot = { ...b, createdAt: b.createdAt ?? Date.now() };
  await store.set(botKey(full.botId), full);
  await store.sadd(botsByTenantKey(full.tenantId), full.botId);
  return full;
}

export async function getAgentBot(botId: string): Promise<AgentTelegramBot | null> {
  return getStore().get<AgentTelegramBot>(botKey(botId));
}

export async function listAgentBotsByTenant(tenantId: string): Promise<AgentTelegramBot[]> {
  const store = getStore();
  const ids = await store.smembers(botsByTenantKey(tenantId));
  const out: AgentTelegramBot[] = [];
  for (const id of ids) {
    const b = await getAgentBot(id);
    if (b) out.push(b);
  }
  return out;
}

export async function deleteAgentBot(botId: string): Promise<void> {
  const store = getStore();
  const b = await getAgentBot(botId);
  if (!b) return;
  await store.del(botKey(botId));
  await store.srem(botsByTenantKey(b.tenantId), botId);
}
