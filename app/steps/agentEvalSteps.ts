// app/steps/agentEvalSteps.ts
//
// Durable "use step" units for per-agent evaluation and the governed
// self-optimization loop (see app/workflows/agentOptimizeWorkflow.ts).
//
// Scoring is an LLM grader that rates an agent's output across named quality
// dimensions (0..100) relevant to that agent's specialty — the same shape the
// eval graph renders. The optimization loop proposes a persona/skills tweak,
// A/B-tests baseline vs. candidate on an identical probe task using a pure,
// side-effect-free generateText (persona-as-system, no tools), scores both on
// the SAME dimensions, and promotes the candidate ONLY when it clears the
// baseline by a margin.

import { generateObject, generateText } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import {
  getSubAgent,
  putSubAgent,
  listAgentsByTenant,
  type SubAgent,
} from "@/app/lib/agents";
import {
  putAgentEvalScore,
  listAgentEvalScores,
  getAgentExperiment,
  putAgentExperiment,
  shouldPromote,
  weeklyOverall,
  DEFAULT_MARGIN,
  type AgentEvalScore,
  type EvalDimension,
} from "@/app/lib/agentEvals";

// Cap text we feed graders/probes so a runaway output can't blow the budget.
const OUTPUT_CLIP = 6000;

function llmCore(purpose: "meta" | "fast-meta", temperature?: number) {
  const llm = buildLlmArgs({ purpose, ...(temperature != null ? { temperature } : {}) });
  return {
    model: (llm as any).model,
    ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
    ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
  };
}

// --- scoring ----------------------------------------------------------------

const scoreSchema = z.object({
  dimensions: z
    .array(z.object({ name: z.string(), score: z.number() }))
    .min(1)
    .max(6),
  note: z.string(),
});

// Grade one agent output. When `dimensions` is provided the grader MUST score
// exactly those (keeps A/B arms comparable); otherwise it derives 3-5 quality
// dimensions appropriate to the agent's specialty (gives the dashboard the
// domain-specific labels in the mockups).
export async function scoreAgentOutputStep(args: {
  tenantId: string;
  agentId: string;
  agentName: string;
  persona: string;
  task: string;
  output: string;
  dimensions?: string[];
  runId?: string;
  experimentId?: string;
  arm?: "baseline" | "candidate";
  threshold?: number;
  // Wall-clock of the scored run, when the caller measured it. Lets the eval
  // dashboard chart latency for long-running task evals alongside quality.
  durationMs?: number;
  // Tool-less capability drill (the eval/optimization probe path): the agent
  // worked ONLY from data embedded in the task, with no live integration
  // access. Flips the grader to judge judgment/drafting on the provided inputs
  // instead of penalizing the absence of live data it was never handed. Live
  // workforce-run scoring leaves this false (the agent really did have tools).
  probe?: boolean;
}): Promise<AgentEvalScore> {
  "use step";

  const fixed = (args.dimensions ?? []).filter(Boolean);
  const dimInstruction = fixed.length
    ? `Score EXACTLY these dimensions, by this name, in this order: ${fixed
        .map((d) => `"${d}"`)
        .join(", ")}.`
    : "Choose 3-5 quality dimensions appropriate to THIS agent's specialty " +
      '(e.g. "Email quality", "Lead qualification accuracy", "Response time SLA", ' +
      '"CRM data completeness"). Use concise Title Case names.';

  // Probe path: grade reasoning/drafting on the embedded inputs; don't punish
  // the agent for data it was never given, but DO punish fabricating external
  // data or stalling to ask for inputs the task already contains.
  const probeNote = args.probe
    ? "IMPORTANT: This is a tool-less capability drill. The agent had NO live tool or " +
      "integration access and worked only from data embedded in the TASK. Grade the quality " +
      "of its judgment, labeling, structure, and drafting GIVEN those inputs. Do NOT penalize " +
      "it for not fetching live data it was never handed. DO penalize fabricating specific " +
      "external data (fake emails, records, numbers) not present in the task, and DO penalize " +
      "stalling to ask the user to paste inputs the task already provides. "
    : "";

  const result = await generateObject({
    ...llmCore("fast-meta", 0),
    schema: scoreSchema,
    system:
      "You are a strict eval grader for an autonomous agent. Judge the OUTPUT against the TASK " +
      "and the agent's MISSION. Score each dimension 0-100 where 100 is flawless, 90 is the " +
      "pass threshold, and below 70 is a real defect. Be calibrated and harsh on hallucination, " +
      "scope creep, and unactioned work. " +
      probeNote +
      dimInstruction,
    prompt: [
      `AGENT: ${args.agentName}`,
      `MISSION / PERSONA:\n${args.persona.slice(0, 1500)}`,
      `TASK:\n${args.task.slice(0, 2000)}`,
      `OUTPUT TO GRADE:\n${args.output.slice(0, OUTPUT_CLIP)}`,
    ].join("\n\n"),
  });

  const dims: EvalDimension[] = result.object.dimensions.map((d) => ({
    name: d.name,
    score: Math.max(0, Math.min(100, Math.round(d.score))),
  }));

  return putAgentEvalScore({
    tenantId: args.tenantId,
    agentId: args.agentId,
    dimensions: dims,
    threshold: args.threshold,
    runId: args.runId,
    experimentId: args.experimentId,
    arm: args.arm,
    note: result.object.note.slice(0, 280),
    ...(typeof args.durationMs === "number" ? { durationMs: args.durationMs } : {}),
  });
}

// --- experiment proposal ----------------------------------------------------

const proposeSchema = z.object({
  hypothesis: z.string(),
  newPersona: z.string(),
  probeTask: z.string(),
  dimensions: z.array(z.string()).min(2).max(6),
});

export async function proposeExperimentStep(args: {
  agentId: string;
}): Promise<{
  experimentId: string;
  tenantId: string;
  agentName: string;
  baselinePersona: string;
  candidatePersona: string;
  probeTask: string;
  dimensions: string[];
} | null> {
  "use step";

  const agent = await getSubAgent(args.agentId);
  if (!agent) return null;

  const recent = await listAgentEvalScores(args.agentId, 30);
  // Surface the weakest recent dimensions so the optimizer targets them.
  const dimAgg = new Map<string, { sum: number; n: number }>();
  for (const s of recent) {
    for (const d of s.dimensions) {
      const cur = dimAgg.get(d.name) ?? { sum: 0, n: 0 };
      cur.sum += d.score;
      cur.n += 1;
      dimAgg.set(d.name, cur);
    }
  }
  const weak = [...dimAgg.entries()]
    .map(([name, v]) => ({ name, avg: v.sum / v.n }))
    .sort((a, b) => a.avg - b.avg)
    .slice(0, 4);
  const weakBlock = weak.length
    ? "Recent dimension averages (target the lowest):\n" +
      weak.map((w) => `- ${w.name}: ${Math.round(w.avg)}`).join("\n")
    : "No eval history yet — propose a generally stronger persona.";

  const result = await generateObject({
    ...llmCore("meta", 0.4),
    schema: proposeSchema,
    system:
      "You optimize an autonomous specialist agent by rewriting its persona/system prompt. " +
      "Propose ONE focused improvement that should raise eval scores without changing the " +
      "agent's core job or its toolkit scope. Return: a one-line hypothesis; the FULL rewritten " +
      "persona (newPersona) — a complete replacement, not a diff; a representative probeTask the " +
      "agent would realistically face (used to A/B test the change); and the list of dimensions " +
      "to grade both variants on. Keep newPersona tight and operational.",
    prompt: [
      `AGENT: ${agent.emoji} ${agent.name}`,
      `CURRENT PERSONA:\n${agent.persona}`,
      `ALLOWED TOOLKITS: [${agent.toolkits.join(", ")}]`,
      weakBlock,
    ].join("\n\n"),
  });

  const exp = await putAgentExperiment({
    tenantId: agent.tenantId,
    agentId: agent.id,
    hypothesis: result.object.hypothesis.slice(0, 280),
    change: { persona: result.object.newPersona },
    status: "testing",
  });

  return {
    experimentId: exp.id,
    tenantId: agent.tenantId,
    agentName: agent.name,
    baselinePersona: agent.persona,
    candidatePersona: result.object.newPersona,
    probeTask: result.object.probeTask,
    dimensions: result.object.dimensions,
  };
}

// --- A/B probe (pure, side-effect-free) -------------------------------------

// Run the agent's persona against the probe task with NO tools, so the only
// variable under test is the persona itself and nothing external is touched.
export async function probeArmStep(args: {
  agentName: string;
  persona: string;
  probeTask: string;
}): Promise<string> {
  "use step";
  const r = await generateText({
    ...llmCore("meta", 0.3),
    system:
      `You are ${args.agentName}. ${args.persona}\n\n` +
      "Produce the actual deliverable the task asks for (draft form is fine — do not claim to " +
      "have sent or executed anything). Be concrete and complete. You have NO live tool access " +
      "in this drill: work ONLY from the information embedded in the task. Do not fabricate " +
      "external data (emails, records, numbers) you were not given, and do not stall asking the " +
      "user to paste inputs the task already provides — just act on what's there.",
    prompt: args.probeTask,
  });
  return (r.text ?? "").slice(0, OUTPUT_CLIP);
}

// --- on-demand single-agent eval --------------------------------------------

// Run ONE eval for an agent right now: synthesize a representative task from
// its mission, produce a side-effect-free deliverable through its persona, and
// score it on derived domain dimensions. Used by the manual "eval my
// workforces" trigger so the eval graph populates without waiting for live
// workforce runs to feed scoreAgentOutputStep organically.
export async function evalAgentStep(args: {
  agentId: string;
  runId?: string;
}): Promise<{
  ok: boolean;
  agentId: string;
  name?: string;
  overall?: number;
  dimensions?: Array<{ name: string; score: number }>;
  error?: string;
}> {
  "use step";

  const agent = await getSubAgent(args.agentId);
  if (!agent) return { ok: false, agentId: args.agentId, error: "agent not found" };

  try {
    const taskGen = await generateText({
      ...llmCore("fast-meta", 0.4),
      system:
        "Write ONE concrete, realistic task this autonomous specialist agent would face. " +
        "CRITICAL: the agent will work WITHOUT any live tool or integration access, so the " +
        "task MUST be fully self-contained — embed every input the agent needs directly in " +
        "the task text (e.g. paste the actual email thread, the CRM record, the message, the " +
        "data rows). NEVER write a task that requires fetching or 'checking' a live system " +
        "(no 'triage the inbox', no 'pull the latest leads') — instead include a realistic " +
        "sample of that data inline and ask the agent to act on it. Output ONLY the task.",
      prompt: [
        `AGENT: ${agent.emoji} ${agent.name}`,
        `MISSION:\n${agent.persona}`,
        `TOOLKITS: [${agent.toolkits.join(", ")}]`,
      ].join("\n\n"),
    });
    const probeTask =
      (taskGen.text ?? "").trim().slice(0, 2000) ||
      "Handle a representative task in your area of responsibility.";

    const output = await probeArmStep({
      agentName: agent.name,
      persona: agent.persona,
      probeTask,
    });

    const score = await scoreAgentOutputStep({
      tenantId: agent.tenantId,
      agentId: agent.id,
      agentName: agent.name,
      persona: agent.persona,
      task: probeTask,
      output,
      probe: true,
      ...(args.runId ? { runId: args.runId } : {}),
    });

    return {
      ok: true,
      agentId: agent.id,
      name: agent.name,
      overall: score.overall,
      dimensions: score.dimensions,
    };
  } catch (err: any) {
    return {
      ok: false,
      agentId: args.agentId,
      name: agent.name,
      error: String(err?.message ?? err).slice(0, 200),
    };
  }
}

// --- decision / promotion ---------------------------------------------------

export async function decideExperimentStep(args: {
  experimentId: string;
  baselineScore: number;
  candidateScore: number;
}): Promise<{
  promoted: boolean;
  baselineScore: number;
  candidateScore: number;
  margin: number;
}> {
  "use step";
  const exp = await getAgentExperiment(args.experimentId);
  if (!exp) {
    return {
      promoted: false,
      baselineScore: args.baselineScore,
      candidateScore: args.candidateScore,
      margin: DEFAULT_MARGIN,
    };
  }

  const promote = shouldPromote(args.baselineScore, args.candidateScore, exp.margin);

  if (promote && exp.change.persona) {
    const agent = await getSubAgent(exp.agentId);
    if (agent) {
      const next: SubAgent = {
        ...agent,
        persona: exp.change.persona,
        skills: exp.change.skills ?? agent.skills,
      };
      await putSubAgent(next);
    }
  }

  await putAgentExperiment({
    ...exp,
    baselineScore: args.baselineScore,
    candidateScore: args.candidateScore,
    status: promote ? "promoted" : "rejected",
    decisionNote: promote
      ? `candidate ${args.candidateScore} beat baseline ${args.baselineScore} by ≥${exp.margin} — promoted`
      : `candidate ${args.candidateScore} did not beat baseline ${args.baselineScore} by ${exp.margin} — kept baseline`,
    decidedAt: Date.now(),
  });

  return {
    promoted: promote,
    baselineScore: args.baselineScore,
    candidateScore: args.candidateScore,
    margin: exp.margin,
  };
}

// --- periodic governed sweep ------------------------------------------------

// How long to wait between automatic optimization runs for one agent. The loop
// is governed (only promotes on a real eval win) but each run still costs LLM
// calls, so we throttle per-agent rather than re-optimizing every cron tick.
const OPT_COOLDOWN_MS = 6 * 60 * 60 * 1000;
const optLastKey = (agentId: string) => `agentopt:last:${agentId}`;

// Walk one tenant's agents and kick off a governed optimization for each one
// that (a) has eval history to target and (b) is past its cooldown. Called once
// per cron tick from the daemon; a no-op for tenants with no eligible agents.
export async function sweepAgentOptimizationStep(args: {
  tenantId: string;
}): Promise<{ launched: string[] }> {
  "use step";
  // Off by default: the self-optimization loop runs on the pricey `meta` tier
  // (gpt-5.4-class) and fires unprompted in the background, so we don't burn
  // tokens on it unless a tenant explicitly opts in. Set AGENT_SELF_OPTIMIZE=1
  // to re-enable the governed A/B persona tuning.
  if (env("AGENT_SELF_OPTIMIZE") !== "1") {
    return { launched: [] };
  }
  const store = getStore();
  const agents = await listAgentsByTenant(args.tenantId);
  const launched: string[] = [];
  const now = Date.now();

  for (const agent of agents) {
    try {
      const lastRaw = await store.get<string | number>(optLastKey(agent.id));
      const last = typeof lastRaw === "number" ? lastRaw : Number(lastRaw ?? 0);
      if (Number.isFinite(last) && last > 0 && now - last < OPT_COOLDOWN_MS) {
        continue;
      }
      // Only optimize agents we have something to learn from — no history
      // means no weak dimensions to target, so a probe would be guesswork.
      const history = await listAgentEvalScores(agent.id, 1);
      if (!history.length) continue;

      await store.set(optLastKey(agent.id), now);
      const { start } = await import("workflow/api");
      const { agentOptimizeWorkflow } = await import(
        "@/app/workflows/agentOptimizeWorkflow"
      );
      await start(agentOptimizeWorkflow, [agent.id]);
      launched.push(agent.id);
    } catch {
      // One agent's optimization launch failing shouldn't abort the sweep.
    }
  }

  return { launched };
}

// Re-exported for callers that want the aggregation without importing the lib.
export { weeklyOverall };
