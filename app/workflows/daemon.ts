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

  // Run for ~285 seconds then exit; cron (*/5 * * * *) restarts every 5 min.
  // We aim to finish just before the next cron tick so daemon:lock can refresh
  // without ever overlapping two daemons.
  //
  // Why this shape (was 50 iters × 1s / 1-min cron): the drain loop is what
  // keeps scheduled-automation/task latency low, and it runs regardless of cron
  // cadence — so covering a 5-min window in ONE daemon lets us cut cron starts
  // 5× (and the once-per-tick heartbeat fan-out + custom-trigger polling) with
  // only a small latency cost.
  //
  // Key constraint: durable `sleep` suspends/resumes the workflow, and each
  // resume REPLAYS all prior loop steps from the journal — so cost grows with
  // ITERATION COUNT, not wall-clock. To cover 5 min without exploding the
  // replay history (the original kept it ~50), we widen the sleep to 5s and
  // keep ~57 iterations. Trade-off: scheduled tasks/automations now fire within
  // ~5s instead of ~1s — imperceptible for time-based triggers (chat-triggered
  // automations fire inline, not via this loop, so they're unaffected).
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

  // Step 2: drain scheduled tasks + due automations. 57 iterations × 5s ≈ 285s
  // wall-clock, staying just under the 300s cron cadence while keeping the
  // replay history small (see header note on why iteration count, not
  // wall-clock, is the cost driver).
  for (let i = 0; i < 57; i++) {
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
