// app/steps/pollCustomTriggersStep.ts
//
// Workflow step wrapper around the custom-trigger poller. Runs once per daemon
// cron tick: polls every local polling-trigger subscription whose interval has
// elapsed and delivers new/matching events to chat. No-op when nobody is
// subscribed.

import { pollDueCustomSubscriptions } from "@/app/lib/customTriggers";

export async function pollCustomTriggersStep(): Promise<{
  polled: number;
  delivered: number;
}> {
  "use step";

  const res = await pollDueCustomSubscriptions({ maxSubs: 25 });
  if (res.polled > 0) {
    console.log(
      `[pollCustomTriggers] polled=${res.polled} delivered=${res.delivered}`
    );
  }
  return res;
}
