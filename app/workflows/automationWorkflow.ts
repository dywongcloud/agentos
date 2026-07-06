// app/workflows/automationWorkflow.ts
//
// Durable runner for one automation firing. Every trigger source (schedule,
// Composio event, webhook, chat keyword, or manual /automate run) funnels
// through fireAutomation(), which records an AutomationRun and starts this
// workflow with the runId.
//
// Two execution modes, decided by the rule's compiled action:
//   - "light": a small ordered list of pure send / VFS ops — run inline as a
//     single durable step. Cheap; no model spend.
//   - "job":   delegate to the full jobWorkflow (clarify/plan/orchestrate/
//     verify, all agent tools + skills). We launch it, then poll its meta to
//     completion via WDK durable sleep so the automation run mirrors the job's
//     terminal status.
//
// Fault tolerance is the same as jobWorkflow: WDK checkpoint/replay + per-step
// retries. The whole body is wrapped so any failure marks the run "error",
// persists it, and surfaces it in /ui/automations. A one-shot manual retry is
// available via `/automate run <id>`.

import { sleep } from "workflow";

import {
  loadAutomationRunStep,
  runLightStepsStep,
  runPlanStepsStep,
  createAutomationJobStep,
  prepareAutomationTurnStep,
  finishAutomationTurnStep,
  pollJobStep,
  finalizeAutomationRunStep,
} from "@/app/steps/automationRunSteps";
import { executeAgentTurnStep } from "@/app/steps/jobSteps";
import { runWorkforceStages } from "@/app/workflows/workforceWorkflow";

// How long to wait between job-status polls, and the max number of polls. Deep
// jobs can run 30-60 min; 120 polls × 30s = 60 min ceiling before we give up
// and finalize with whatever the job last reported.
const POLL_INTERVAL = "30s";
const MAX_POLLS = 120;

export async function automationWorkflow(runId: string) {
  "use workflow";

  const { run, rule } = await loadAutomationRunStep(runId);
  if (!run || !rule) return;

  try {
    // Workforce mode: run the team's stages (parallel member agent turns per
    // stage, optional AI-routing stages); finalize delivers the composed
    // summary to the team's channel.
    if (rule.action.mode === "workforce") {
      const resultText = await runWorkforceStages(runId);
      await finalizeAutomationRunStep({ runId, status: "ok", resultText });
      return;
    }

    if (rule.action.mode === "light") {
      const resultText = await runLightStepsStep({ runId });
      await finalizeAutomationRunStep({ runId, status: "ok", resultText });
      return;
    }

    // Plan mode: a deterministic tool-call workflow. Execute the compiled step
    // list directly — no agent turn, no tool-search — spending model tokens
    // only on `ai`-typed fields (e.g. an email body). This is the durable path
    // for the common case where the tool + parameters are fixed at compile time.
    if (rule.action.mode === "plan") {
      const resultText = await runPlanStepsStep({ runId });
      await finalizeAutomationRunStep({ runId, status: "ok", resultText });
      return;
    }

    // Non-deep job mode: run EXACTLY ONE agent turn. No clarify/plan/verify/
    // revise loop, so a side-effecting action (append a sheet row, send a
    // message) runs once and the user gets a single result message instead of
    // the verifier re-executing it ~7 times. The agent turn delivers its own
    // answer; finalize adds the compact status footer.
    if (!rule.action.deep) {
      const ctx = await prepareAutomationTurnStep({ runId });
      const out = await executeAgentTurnStep({
        jobId: ctx.jobId,
        tenantId: ctx.tenantId,
        sessionId: ctx.sessionId,
        channel: ctx.channel,
        prompt: ctx.prompt,
        showTyping: ctx.channel === "telegram",
        modelName: ctx.modelName,
      });
      await finishAutomationTurnStep({ jobId: ctx.jobId, text: out.text });
      await finalizeAutomationRunStep({ runId, status: "ok", resultText: out.text });
      return;
    }

    // Deep job mode: launch the full durable job (createAutomationJobStep
    // starts the jobWorkflow from inside the step), then poll its meta to
    // completion — orchestration is appropriate for genuinely multi-step work.
    const jobId = await createAutomationJobStep({ runId });

    let last = await pollJobStep({ jobId });
    let polls = 0;
    while (!last.terminal && polls < MAX_POLLS) {
      await sleep(POLL_INTERVAL);
      last = await pollJobStep({ jobId });
      polls++;
    }

    if (!last.terminal) {
      await finalizeAutomationRunStep({
        runId,
        status: "error",
        error: `job ${jobId} did not finish within the poll window`,
      });
      return;
    }

    await finalizeAutomationRunStep({
      runId,
      status: last.status === "done" ? "ok" : "error",
      resultText: last.resultText,
      error: last.status === "done" ? undefined : last.error || `job ${last.status}`,
    });
  } catch (err: any) {
    await finalizeAutomationRunStep({
      runId,
      status: "error",
      error: String(err?.message ?? err).slice(0, 400),
    });
  }
}
