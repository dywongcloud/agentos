// app/workflows/bulkWorkflow.ts
//
// Durable driver for one bulk run (see app/lib/bulkRuns.ts). Fetches the row
// set once, then walks it in small checkpointed batches with WDK durable
// sleeps in between — a 1000-row run is ~200 five-row steps, each finishing in
// seconds, so no single invocation can time out and a crash resumes where it
// left off. Per-item idempotency lives in the steps; this loop only sequences.

import { sleep } from "workflow";

import {
  loadBulkRunStep,
  fetchBulkItemsStep,
  processBulkBatchStep,
  notifyBulkProgressStep,
  finalizeBulkRunStep,
  failBulkRunStep,
} from "@/app/steps/bulkRunSteps";

// Pause between batches — keeps well under provider rate limits (Gmail is
// comfortable around ~1 send/sec; per-item delay inside the batch adds more).
const BATCH_PAUSE = "2s";
// Progress ping cadence (items).
const PING_EVERY = 100;

export async function bulkWorkflow(runId: string) {
  "use workflow";

  const run = await loadBulkRunStep(runId);
  if (!run) return;

  try {
    const total = await fetchBulkItemsStep(runId);
    if (total === 0) {
      await failBulkRunStep({ runId, error: "fetch returned no rows" });
      return;
    }

    await notifyBulkProgressStep({
      runId,
      note:
        `📦 Bulk run ${runId} started: ${run.description}\n` +
        `${total} row(s) to process${run.dryRun ? " (DRY RUN — nothing will actually be sent)" : ""}. ` +
        `I'll report progress every ${PING_EVERY} and post a final summary. /stop halts it.`,
    });

    let start = 0;
    let lastPingBucket = 0;
    let halted = false;
    let haltReason: string | undefined;

    while (start < total) {
      const res = await processBulkBatchStep({ runId, start });
      start += Math.max(1, res.processed || run.batchSize);

      if (res.halted) {
        halted = true;
        haltReason = res.haltReason;
        break;
      }

      const processedSoFar = res.done + res.failed + res.skipped;
      const bucket = Math.floor(processedSoFar / PING_EVERY);
      if (bucket > lastPingBucket) {
        lastPingBucket = bucket;
        await notifyBulkProgressStep({
          runId,
          note: `📦 ${runId}: ${processedSoFar}/${total} processed (${res.done} ok, ${res.failed} failed, ${res.skipped} skipped)…`,
        });
      }

      if (start < total) await sleep(BATCH_PAUSE);
    }

    await finalizeBulkRunStep({ runId, halted, haltReason });
  } catch (err: any) {
    await failBulkRunStep({ runId, error: err?.message ?? String(err) });
  }
}
