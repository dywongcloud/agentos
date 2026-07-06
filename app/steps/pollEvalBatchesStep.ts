// app/steps/pollEvalBatchesStep.ts
//
// Workflow step wrapper around the model-compare Batch poller. Runs once per
// daemon cron tick (only when EVALS_USE_BATCH is on): checks any pending OpenAI
// batches and finalizes their deferred judge cells. No-op when none are pending.

import { env } from "@/app/lib/env";
import { pollCompareBatches } from "@/app/lib/evals/batchCompare";

export async function pollEvalBatchesStep(): Promise<{
  checked: number;
  completed: number;
  stillRunning: number;
  finalizedCells: number;
}> {
  "use step";

  if (!env("EVALS_USE_BATCH")) {
    return { checked: 0, completed: 0, stillRunning: 0, finalizedCells: 0 };
  }

  const res = await pollCompareBatches({ maxBatches: 5 });
  if (res.checked > 0) {
    console.log(
      `[pollEvalBatches] checked=${res.checked} completed=${res.completed} running=${res.stillRunning} finalized=${res.finalizedCells}`
    );
  }
  return res;
}
