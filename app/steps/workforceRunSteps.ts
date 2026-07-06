// app/steps/workforceRunSteps.ts
//
// Durable "use step" units for one workforce (team) run. Orchestrated by
// runWorkforceStages() in app/workflows/workforceWorkflow.ts, which is called
// from inside automationWorkflow when a rule's action is
// { mode: "workforce", workforceId }.
//
// Stage records accumulate in Redis under wfrun:{automationRunId} so later
// stages (and the /ui canvas) can read earlier outputs.

import { generateObject } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import {
  getRun,
  getAutomation,
  appendRunThought,
  summarizeEvent,
  type Automation,
  type AutomationRun,
} from "@/app/lib/automations";
import { createJob, updateJobMeta } from "@/app/lib/jobStore";
import {
  getWorkforce,
  getAgentsByIds,
  getSubAgent,
  getStageRecords,
  appendStageRecord,
  putWorkforce,
  scopeForAgent,
  type Workforce,
  type SubAgent,
  type SubAgentScope,
  type WorkforceStage,
  type WorkforceStageRecord,
} from "@/app/lib/agents";
import type { Channel } from "@/app/lib/identity";
import { recallContext, addMemory } from "@/app/lib/agentMemory";

// Per-teammate output embedded into later-stage prompts; keeps a 4-agent
// stage's combined context well under the model budget.
const PRIOR_OUTPUT_CLIP = 4000;

async function loadRunContext(runId: string): Promise<{
  run: AutomationRun;
  rule: Automation;
  team: Workforce;
}> {
  const run = await getRun(runId);
  if (!run) throw new Error(`run not found: ${runId}`);
  const rule = await getAutomation(run.automationId);
  if (!rule || rule.action.mode !== "workforce") {
    throw new Error(`run ${runId} is not a workforce automation`);
  }
  const team = await getWorkforce(rule.action.workforceId);
  if (!team) throw new Error(`workforce not found: ${rule.action.workforceId}`);
  return { run, rule, team };
}

// --- load -------------------------------------------------------------------

export async function loadWorkforceRunStep(args: { runId: string }): Promise<{
  teamId: string;
  teamName: string;
  stages: WorkforceStage[];
}> {
  "use step";
  const { team } = await loadRunContext(args.runId);
  await putWorkforce({ ...team, lastRunId: args.runId });
  await appendRunThought(args.runId, {
    kind: "step",
    text: `workforce "${team.name}" run started — ${team.stages.length} stage(s)`,
  });
  return { teamId: team.id, teamName: team.name, stages: team.stages };
}

// --- member turns -------------------------------------------------------------

function eventBlock(run: AutomationRun): string {
  const summary = run.eventSummary ?? summarizeEvent(run.event);
  const parts: string[] = [];
  if (summary.id) {
    parts.push(`CURRENT EVENT ID: ${summary.id}`);
  }
  if (summary.fields) {
    parts.push(
      `Key fields from the triggering event (source: ${run.source}):\n\`\`\`json\n${summary.fields}\n\`\`\``
    );
  }
  return parts.join("\n");
}

function priorOutputsBlock(records: WorkforceStageRecord[]): string {
  if (!records.length) return "";
  const parts: string[] = ["TEAMMATE OUTPUTS FROM EARLIER STAGES (build on these; do not redo their work):"];
  for (const rec of records) {
    for (const out of rec.outputs) {
      parts.push(
        `--- stage ${rec.stageIndex + 1} · ${out.agentName} (${out.status}) ---\n` +
          out.text.slice(0, PRIOR_OUTPUT_CLIP)
      );
    }
  }
  return parts.join("\n\n");
}

function buildMemberPrompt(args: {
  team: Workforce;
  agent: SubAgent;
  run: AutomationRun;
  records: WorkforceStageRecord[];
  stageIndex: number;
}): string {
  const { team, agent, run, records, stageIndex } = args;
  const parts: string[] = [];

  parts.push(
    `You are ${agent.emoji} ${agent.name}, a specialist member of the team "${team.name}". ` +
      `A team workflow run is in progress and stage ${stageIndex + 1} is your turn to act. ` +
      "PERFORM your part right now by calling your live tools (Composio app tools, VFS, etc.) " +
      "against the event and teammate outputs below. Do NOT write or send source code, a handler " +
      "function, or pseudo-code describing how the work WOULD be done — act directly and report " +
      "what you actually did."
  );

  parts.push(`TEAM MISSION:\n${team.spec.trim()}`);

  const ev = eventBlock(run);
  if (ev) parts.push(ev);

  const prior = priorOutputsBlock(records);
  if (prior) parts.push(prior);

  parts.push(
    `YOUR TASK: carry out the part of the mission that matches your specialty (${agent.persona.trim()}), ` +
      "building on your teammates' outputs above. Stay strictly within your specialty and your allowed " +
      "toolkits; if a needed action is out of your scope, say so in your report instead of attempting it. " +
      `When done, reply with a concise report addressed to your teammates — what you did, key findings, ` +
      `and any ids/links they will need (the next stage reads your reply verbatim). Sign off as ${agent.name}.`
  );

  return parts.join("\n\n");
}

export async function prepareMemberTurnStep(args: {
  runId: string;
  stageIndex: number;
  agentId: string;
}): Promise<{
  jobId: string;
  tenantId: string;
  sessionId: string;
  channel: Channel;
  prompt: string;
  agent: SubAgentScope;
}> {
  "use step";
  const { run, rule, team } = await loadRunContext(args.runId);
  const agent = await getSubAgent(args.agentId);
  if (!agent) throw new Error(`sub-agent not found: ${args.agentId}`);

  const records = await getStageRecords(args.runId);
  let prompt = buildMemberPrompt({
    team,
    agent,
    run,
    records,
    stageIndex: args.stageIndex,
  });

  // Pull the most relevant shared/team/private memories + knowledgebase for
  // this task and prepend them, so the agent benefits from what it (and its
  // teammates) learned on prior runs. Best-effort: a memory hiccup must not
  // block the turn.
  try {
    const recalled = await recallContext({
      tenantId: rule.tenantId,
      agentId: agent.id,
      workforceId: team.id,
      query: `${team.spec}\n${agent.persona}`.slice(0, 1500),
      topK: 6,
    });
    if (recalled) prompt = `${recalled}\n\n${prompt}`;
  } catch {
    // recall failure is non-fatal
  }

  // Fresh per-member session per run: isolated history, isolated VFS diff.
  const sessionId = `wf:${team.id}:${args.runId}:${agent.id}`;
  const meta = await createJob({
    tenantId: rule.tenantId,
    channel: rule.channel,
    sessionId,
    prompt,
    kind: "auto",
    agentId: agent.id,
    workforceRunId: args.runId,
  });

  await appendRunThought(args.runId, {
    kind: "step",
    text: `stage ${args.stageIndex + 1}: ${agent.emoji} ${agent.name} → job ${meta.jobId}`,
  });

  return {
    jobId: meta.jobId,
    tenantId: rule.tenantId,
    sessionId,
    channel: rule.channel,
    prompt,
    agent: scopeForAgent(agent),
  };
}

export async function finishMemberTurnStep(args: {
  jobId: string;
  text: string;
  ok: boolean;
  error?: string;
}): Promise<void> {
  "use step";
  await updateJobMeta(args.jobId, {
    status: args.ok ? "done" : "failed",
    resultText: args.text || undefined,
    error: args.error,
  });
}

// Persist what a member learned this turn as a durable takeaway: written to the
// agent's PRIVATE memory (so the same specialist improves run over run) and to
// the TEAM memory (so teammates can recall it next time). Embedding happens
// here inside a step. Best-effort; failures must not fail the run.
export async function recordMemberMemoryStep(args: {
  tenantId: string;
  workforceId: string;
  agentId: string;
  agentName: string;
  task: string;
  output: string;
}): Promise<void> {
  "use step";
  const text = args.output.trim();
  if (!text) return;
  const clipped = text.slice(0, 1200);
  try {
    await addMemory({
      tenantId: args.tenantId,
      scope: { kind: "agent", agentId: args.agentId },
      kind: "takeaway",
      text: clipped,
      source: `run takeaway`,
    });
    await addMemory({
      tenantId: args.tenantId,
      scope: { kind: "workforce", workforceId: args.workforceId },
      kind: "takeaway",
      text: `[${args.agentName}] ${clipped}`,
      source: `${args.agentName}`,
    });
  } catch {
    // memory write failure is non-fatal
  }
}

// --- stage records -------------------------------------------------------------

export async function recordStageStep(args: {
  runId: string;
  record: Omit<WorkforceStageRecord, "ts">;
}): Promise<void> {
  "use step";
  await appendStageRecord(args.runId, { ...args.record, ts: Date.now() });
  const ok = args.record.outputs.filter((o) => o.status === "ok").length;
  await appendRunThought(args.runId, {
    kind: "step",
    text: `stage ${args.record.stageIndex + 1} done: ${ok}/${args.record.outputs.length} member(s) ok`,
  });
}

// --- route stages ----------------------------------------------------------------

const routeSchema = z.object({
  agentIds: z.array(z.string()),
  reason: z.string(),
});

export async function routeStageStep(args: {
  runId: string;
  stageIndex: number;
}): Promise<string[]> {
  "use step";
  const { run, team } = await loadRunContext(args.runId);
  const stage = team.stages[args.stageIndex];
  if (!stage || stage.kind !== "route") {
    throw new Error(`stage ${args.stageIndex} of team ${team.id} is not a route stage`);
  }

  const candidates = await getAgentsByIds(stage.candidateAgentIds);
  if (!candidates.length) throw new Error(`route stage has no resolvable candidates`);
  const maxPick = Math.max(1, stage.maxPick ?? 1);

  const records = await getStageRecords(args.runId);
  const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0 });
  const result = await generateObject({
    model: (llm as any).model,
    ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
    ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
    schema: routeSchema,
    system:
      "You are a router for a team of specialist agents. Pick which agent(s) should handle " +
      "this stage of the workflow run. Return ONLY agent ids from the candidate list, " +
      `at most ${maxPick}. Prefer the single best fit unless the instruction demands more.`,
    prompt: [
      `Routing instruction: ${stage.instruction}`,
      eventBlock(run) || "(no event payload)",
      priorOutputsBlock(records) || "(no prior stage outputs)",
      "Candidates:",
      ...candidates.map(
        (c) =>
          `- id=${c.id} name="${c.name}" specialty="${c.persona.slice(0, 200)}" toolkits=[${c.toolkits.join(", ")}]`
      ),
    ].join("\n\n"),
  });

  const valid = new Set(candidates.map((c) => c.id));
  let picked = (result.object.agentIds ?? []).filter((id) => valid.has(id));
  if (!picked.length) picked = [candidates[0].id];
  picked = picked.slice(0, maxPick);

  await appendRunThought(args.runId, {
    kind: "step",
    text: `stage ${args.stageIndex + 1} routed → ${picked.join(", ")} (${result.object.reason.slice(0, 160)})`,
  });
  return picked;
}

// --- final summary ----------------------------------------------------------------

export async function composeWorkforceSummaryStep(args: {
  runId: string;
}): Promise<string> {
  "use step";
  const { team } = await loadRunContext(args.runId);
  const records = await getStageRecords(args.runId);

  const parts: string[] = [
    `🤝 Workforce "${team.name}" finished ${records.length}/${team.stages.length} stage(s).`,
  ];
  for (const rec of records) {
    for (const out of rec.outputs) {
      const icon = out.status === "ok" ? "✅" : "⚠️";
      parts.push(
        `${icon} Stage ${rec.stageIndex + 1} · ${out.agentName}:\n${out.text.slice(0, 400).trim()}`
      );
    }
  }
  let text = parts.join("\n\n");
  if (text.length > 3500) text = text.slice(0, 3480) + "\n…(truncated)";
  return text;
}
