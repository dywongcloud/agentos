// app/workflows/agentOptimizeWorkflow.ts
//
// Governed self-optimization for one sub-agent (the "L4 self-driving" loop):
//   1. propose ONE persona improvement targeting the weakest eval dimensions
//   2. A/B-test baseline vs. candidate on an identical probe task (pure,
//      tool-less generation — no external side effects)
//   3. score both arms on the SAME dimensions
//   4. promote the candidate ONLY if it beats the baseline by the margin
//
// Started via launchAgentOptimization() (from the cron tick or the
// `/agent optimize` command). Plain WDK workflow: it only calls steps.

import {
  proposeExperimentStep,
  probeArmStep,
  scoreAgentOutputStep,
  decideExperimentStep,
} from "@/app/steps/agentEvalSteps";

export async function agentOptimizeWorkflow(agentId: string) {
  "use workflow";

  const proposal = await proposeExperimentStep({ agentId });
  if (!proposal) return;

  const [baselineOut, candidateOut] = await Promise.all([
    probeArmStep({
      agentName: proposal.agentName,
      persona: proposal.baselinePersona,
      probeTask: proposal.probeTask,
    }),
    probeArmStep({
      agentName: proposal.agentName,
      persona: proposal.candidatePersona,
      probeTask: proposal.probeTask,
    }),
  ]);

  const [baseline, candidate] = await Promise.all([
    scoreAgentOutputStep({
      tenantId: proposal.tenantId,
      agentId,
      agentName: proposal.agentName,
      persona: proposal.baselinePersona,
      task: proposal.probeTask,
      output: baselineOut,
      dimensions: proposal.dimensions,
      experimentId: proposal.experimentId,
      arm: "baseline",
    }),
    scoreAgentOutputStep({
      tenantId: proposal.tenantId,
      agentId,
      agentName: proposal.agentName,
      persona: proposal.candidatePersona,
      task: proposal.probeTask,
      output: candidateOut,
      dimensions: proposal.dimensions,
      experimentId: proposal.experimentId,
      arm: "candidate",
    }),
  ]);

  await decideExperimentStep({
    experimentId: proposal.experimentId,
    baselineScore: baseline.overall,
    candidateScore: candidate.overall,
  });
}

// Fire-and-forget launcher. Returns true if a run was started.
export async function launchAgentOptimization(agentId: string): Promise<boolean> {
  const { start } = await import("workflow/api");
  await start(agentOptimizeWorkflow, [agentId]);
  return true;
}
