// app/api/ui/agent-evals/route.ts
//
// Data feed for the eval-graph view (rendered standalone and embedded as a tab
// in the workflow dashboard). Tenant-scoped, same auth as the rest of /api/ui.
// Ops:
//   ?op=overview            → every tenant agent: 12-week overall series,
//                             latest dimensions, degradation flag, experiment
//                             counts; plus a fleet-wide weekly series.
//   ?op=agent&agentId=<id>  → one agent: full weekly series, recent score rows
//                             (dimensions), and recent A/B experiments.

import { NextResponse } from "next/server";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import { listAgentsByTenant, getSubAgent } from "@/app/lib/agents";
import {
  listAgentEvalScores,
  listAgentExperiments,
  weeklyOverall,
  DEFAULT_THRESHOLD,
  type AgentEvalScore,
} from "@/app/lib/agentEvals";

export const dynamic = "force-dynamic";

// "Degradation detected": the most recent run scored materially below the agent's
// recent baseline (mean of the prior runs). Mirrors the screenshot's red flag.
const DEGRADE_DROP = 5; // points below trailing mean
const DEGRADE_MIN_HISTORY = 3;

function detectDegradation(scores: AgentEvalScore[]): {
  degraded: boolean;
  delta: number | null;
} {
  if (scores.length < DEGRADE_MIN_HISTORY) return { degraded: false, delta: null };
  const latest = scores[0]!.overall;
  const prior = scores.slice(1, 6);
  const mean = prior.reduce((a, s) => a + s.overall, 0) / prior.length;
  const delta = Math.round((latest - mean) * 10) / 10;
  return { degraded: delta <= -DEGRADE_DROP, delta };
}

export async function GET(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  const tenant = await resolveUiTenant(url.searchParams.get("userId"));
  if (!tenant) {
    return NextResponse.json({ error: "unknown tenant" }, { status: 400 });
  }

  const op = url.searchParams.get("op") ?? "overview";

  if (op === "overview") {
    const agents = await listAgentsByTenant(tenant);
    const now = Date.now();

    const perAgent = await Promise.all(
      agents.map(async (a) => {
        const scores = await listAgentEvalScores(a.id, 200);
        const latest = scores[0] ?? null;
        const { degraded, delta } = detectDegradation(scores);
        return {
          id: a.id,
          name: a.name,
          emoji: a.emoji,
          toolkits: a.toolkits,
          threshold: latest?.threshold ?? DEFAULT_THRESHOLD,
          scoreCount: scores.length,
          latest: latest
            ? {
                overall: latest.overall,
                ts: latest.ts,
                note: latest.note ?? null,
                dimensions: latest.dimensions,
              }
            : null,
          weekly: weeklyOverall(scores, 12, now),
          degraded,
          degradeDelta: delta,
          allScores: scores.map((s) => ({
            overall: s.overall,
            ts: s.ts,
            ...(typeof s.durationMs === "number" ? { durationMs: s.durationMs } : {}),
          })),
        };
      })
    );

    // Fleet-wide weekly series: pool every agent's scores into one 12-week chart.
    const pooled: AgentEvalScore[] = [];
    for (const a of agents) {
      const s = await listAgentEvalScores(a.id, 200);
      pooled.push(...s);
    }
    const fleetWeekly = weeklyOverall(pooled, 12, now);

    return NextResponse.json({
      threshold: DEFAULT_THRESHOLD,
      fleetWeekly,
      agents: perAgent,
    });
  }

  if (op === "agent") {
    const agentId = url.searchParams.get("agentId") ?? "";
    const agent = await getSubAgent(agentId);
    if (!agent || agent.tenantId !== tenant) {
      return NextResponse.json({ error: "unknown agent" }, { status: 404 });
    }
    const [scores, experiments] = await Promise.all([
      listAgentEvalScores(agentId, 200),
      listAgentExperiments(agentId, 30),
    ]);
    const { degraded, delta } = detectDegradation(scores);
    return NextResponse.json({
      agent: {
        id: agent.id,
        name: agent.name,
        emoji: agent.emoji,
        persona: agent.persona,
        toolkits: agent.toolkits,
        telegramBotId: agent.telegramBotId ?? null,
      },
      threshold: scores[0]?.threshold ?? DEFAULT_THRESHOLD,
      weekly: weeklyOverall(scores, 12),
      degraded,
      degradeDelta: delta,
      scores: scores.slice(0, 40).map((s) => ({
        id: s.id,
        overall: s.overall,
        dimensions: s.dimensions,
        threshold: s.threshold,
        note: s.note ?? null,
        arm: s.arm ?? null,
        experimentId: s.experimentId ?? null,
        ts: s.ts,
      })),
      experiments: experiments.map((e) => ({
        id: e.id,
        hypothesis: e.hypothesis,
        status: e.status,
        baselineScore: e.baselineScore ?? null,
        candidateScore: e.candidateScore ?? null,
        margin: e.margin,
        decisionNote: e.decisionNote ?? null,
        createdAt: e.createdAt,
        decidedAt: e.decidedAt ?? null,
      })),
    });
  }

  return NextResponse.json({ error: `unknown op ${op}` }, { status: 400 });
}
