// app/steps/runDueAutomationsStep.ts
//
// Drains the auto:schedule ZSET: every due scheduled automation is fired once,
// then its next occurrence is recomputed (cron / interval) and re-armed — or
// removed if it was a one-shot. Called each tick from the minute-cron daemon
// (app/workflows/daemon.ts), alongside runDueTasks.

import {
  dueScheduleIds,
  getAutomation,
  fireAutomation,
  rescheduleAutomation,
} from "@/app/lib/automations";

export async function runDueAutomationsStep(): Promise<{ fired: number }> {
  "use step";
  const now = Date.now();
  const ids = await dueScheduleIds(now, 25);
  let fired = 0;
  for (const id of ids) {
    const rule = await getAutomation(id);
    if (!rule || !rule.enabled || rule.trigger.kind !== "schedule") {
      // Stale index entry — reschedule will clean it up if the rule exists.
      if (rule) await rescheduleAutomation(rule);
      continue;
    }
    try {
      await fireAutomation(id, "schedule", { firedAt: now, kind: "schedule" });
      fired++;
    } catch {
      // One automation's failure shouldn't block the rest of the drain.
    } finally {
      // Always advance the schedule so a failing rule doesn't busy-loop.
      await rescheduleAutomation(rule);
    }
  }
  return { fired };
}
