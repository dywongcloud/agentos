import { sleep } from "workflow";
import { runDueTasks } from "@/app/steps/runDueTasks";
import { runDueAutomationsStep } from "@/app/steps/runDueAutomationsStep";
import { autopilotHeartbeatStep } from "@/app/steps/autopilotHeartbeatStep";
import { listProactiveTenantsStep } from "@/app/steps/autopilotSteps";
import { pollCustomTriggersStep } from "@/app/steps/pollCustomTriggersStep";
import { pollEvalBatchesStep } from "@/app/steps/pollEvalBatchesStep";
import { sweepAgentOptimizationStep } from "@/app/steps/agentEvalSteps";

export async function daemonWorkflow() {
  "use workflow";

  // Run for ~50 seconds then exit; cron (*/5 * * * *) fires 6 times per
  // 5-minute window, so 6 × 50s gives equivalent coverage to a single long
  // daemon while keeping the replay history shallow.
  //
  // Why 10 iterations (not the old 57): WDK replays ALL prior steps on every
  // durable-sleep resume, so replay cost grows as O(iterations^2). Dropping
  // from 57 to 10 iterations per invocation cuts that quadratic term by ~97%
  // while the 6-invocations-per-window cadence preserves end-to-end coverage.
  //
  // Key constraint: durable `sleep` suspends/resumes the workflow, and each
  // resume REPLAYS all prior loop steps from the journal — so cost grows with
  // ITERATION COUNT, not wall-clock. Keeping iterations low (10) and relying
  // on frequent cron restarts (every 5 min, 6 overlapping invocations) is the
  // right trade-off. Scheduled task/automation latency remains ≤5s.
  // Chat-triggered automations fire inline and are unaffected.
  //
  // Each cron tick fans the autopilot heartbeat out across all opted-in
  // tenants once, then keeps sweeping the task queue until time's up.

  // Step 1: per-tenant heartbeat fan-out. The step itself does the
  // preflight gating (cooldown, quiet hours, recent-user-active) and is
  // a no-op for most ticks. LLM only fires when something looks notable.
  try {
    const tenants = await listProactiveTenantsStep();
    for (const tid of tenants) {
      try {
        await autopilotHeartbeatStep({ tenantId: tid });
      } catch {
        // One tenant's heartbeat failure shouldn't abort the rest.
      }
      try {
        // Governed L4 self-optimization: per-agent throttled A/B test that only
        // promotes a persona tweak when it beats the proven baseline.
        await sweepAgentOptimizationStep({ tenantId: tid });
      } catch {
        // Optimization sweep failure shouldn't abort the heartbeat fan-out.
      }
    }
  } catch {
    // No tenants configured / store unavailable — fine, continue.
  }

  // Step 1.5: poll local custom (polling) trigger subscriptions — e.g.
  // monday.com, which has no native Composio trigger. Each due subscription
  // runs a Composio read action, diffs against last-seen state, and delivers
  // new/matching events to the user's chat. A no-op when nobody's subscribed.
  try {
    await pollCustomTriggersStep();
  } catch {
    // Polling failure shouldn't abort the task-drain below.
  }

  // Step 1.6: finalize any completed model-compare Batch-API jobs (judge grades
  // submitted asynchronously). No-op unless EVALS_USE_BATCH is set and a batch
  // is pending. Async/latency-tolerant by design — once-per-tick is plenty.
  try {
    await pollEvalBatchesStep();
  } catch {
    // Batch poll failure shouldn't abort the task-drain below.
  }

  // Step 2: drain scheduled tasks + due automations. 10 iterations × 5s = 50s
  // wall-clock per invocation; 6 cron invocations per 5-min window give full
  // coverage while capping replay depth at 10 (see header note on why
  // iteration count, not wall-clock, is the cost driver).
  for (let i = 0; i < 10; i++) {
    await runDueTasks();
    try {
      await runDueAutomationsStep();
    } catch {
      // Automation drain failure shouldn't abort the task drain.
    }
    await sleep("5s");
  }

  return { ok: true };
}
