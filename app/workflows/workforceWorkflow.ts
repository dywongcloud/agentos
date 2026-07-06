// app/workflows/workforceWorkflow.ts
//
// Stage loop for one workforce (team) run. NOT a "use workflow" function: it
// is a plain async helper called from INSIDE automationWorkflow's workflow
// context, and only ever awaits "use step" functions — so the WDK checkpoints
// every member turn / stage record individually and replay stays correct.
//
// Semantics: stages execute in order; agents within a stage run in parallel
// (Promise.all over steps is the established WDK pattern); a "route" stage
// first asks a fast meta model to pick which candidate agents act, then runs
// the picked agents like a normal stage. Each stage's outputs are persisted
// to wfrun:{runId} so later stages' prompts (and the /ui canvas) can read
// them. A member failure records an error output but does not abort the run.

import {
  loadWorkforceRunStep,
  prepareMemberTurnStep,
  finishMemberTurnStep,
  recordStageStep,
  routeStageStep,
  composeWorkforceSummaryStep,
  recordMemberMemoryStep,
} from "@/app/steps/workforceRunSteps";
import { executeAgentTurnStep } from "@/app/steps/jobSteps";
import { scoreAgentOutputStep } from "@/app/steps/agentEvalSteps";

// Parallel member turns each carry the full 8-min agent-turn deadline; cap the
// width so one stage can't fan out unboundedly.
const STAGE_WIDTH_CAP = 4;

export async function runWorkforceStages(runId: string): Promise<string> {
  const { stages, teamId } = await loadWorkforceRunStep({ runId });

  for (let i = 0; i < stages.length; i++) {
    const stage = stages[i];

    let agentIds: string[];
    let pickedAgentIds: string[] | undefined;
    if (stage.kind === "route") {
      agentIds = await routeStageStep({ runId, stageIndex: i });
      pickedAgentIds = agentIds;
    } else {
      agentIds = stage.agentIds;
    }
    agentIds = agentIds.slice(0, STAGE_WIDTH_CAP);

    const outputs = await Promise.all(
      agentIds.map(async (agentId) => {
        let ctx: Awaited<ReturnType<typeof prepareMemberTurnStep>> | null = null;
        try {
          ctx = await prepareMemberTurnStep({ runId, stageIndex: i, agentId });
          const startedAt = Date.now();
          const out = await executeAgentTurnStep({
            jobId: ctx.jobId,
            tenantId: ctx.tenantId,
            sessionId: ctx.sessionId,
            channel: ctx.channel,
            prompt: ctx.prompt,
            // Member sessions ("wf:…") are not real chats; never stream or
            // deliver from inside the member turn.
            showTyping: false,
            agent: ctx.agent,
          });
          await finishMemberTurnStep({ jobId: ctx.jobId, text: out.text, ok: true });
          // Score the real run so the eval graph and the governed optimizer
          // have live data to target. Best-effort: a grader hiccup must not
          // fail the member's turn.
          try {
            await scoreAgentOutputStep({
              tenantId: ctx.tenantId,
              agentId,
              agentName: ctx.agent.name,
              persona: ctx.agent.persona,
              task: ctx.prompt,
              output: out.text,
              runId,
              durationMs: Date.now() - startedAt,
            });
          } catch {
            // grader failure is non-fatal
          }
          // Persist the member's takeaway to private + team memory so future
          // runs (and the office view) accumulate institutional knowledge.
          try {
            await recordMemberMemoryStep({
              tenantId: ctx.tenantId,
              workforceId: teamId,
              agentId,
              agentName: ctx.agent.name,
              task: ctx.prompt,
              output: out.text,
            });
          } catch {
            // memory write failure is non-fatal
          }
          return {
            agentId,
            agentName: ctx.agent.name,
            jobId: ctx.jobId,
            status: "ok" as const,
            text: out.text,
          };
        } catch (err) {
          const msg = String((err as { message?: string })?.message ?? err).slice(0, 400);
          if (ctx) {
            await finishMemberTurnStep({ jobId: ctx.jobId, text: "", ok: false, error: msg });
          }
          return {
            agentId,
            agentName: ctx?.agent.name ?? agentId,
            jobId: ctx?.jobId,
            status: "error" as const,
            text: msg,
          };
        }
      })
    );

    await recordStageStep({
      runId,
      record: { stageIndex: i, kind: stage.kind, pickedAgentIds, outputs },
    });
  }

  return composeWorkforceSummaryStep({ runId });
}
