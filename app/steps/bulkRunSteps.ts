// app/steps/bulkRunSteps.ts
//
// Checkpointed "use step" units for bulkWorkflow. Each step is small (one
// fetch, or one batch of ≤25 items) so every invocation finishes far inside
// serverless limits; WDK retries/replays a failed step, and the per-item
// idempotency ledger guarantees a replay never repeats a side effect that
// already succeeded (no double-sent emails).

import {
  getBulkRun,
  patchBulkRun,
  saveBulkItems,
  loadBulkItems,
  getItemStatus,
  setItemStatus,
  recordBulkFailure,
  listBulkFailures,
  normalizeRows,
  resolveRowTemplate,
  type BulkRun,
} from "@/app/lib/bulkRuns";
import { executeComposioAction } from "@/app/lib/composioExec";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { recordActivity } from "@/app/lib/activityLog";
import { isChatStopped } from "@/app/lib/chatControl";

export async function loadBulkRunStep(runId: string): Promise<BulkRun | null> {
  "use step";
  return getBulkRun(runId);
}

// One deterministic READ (e.g. the whole sheet tab), normalized to header-keyed
// rows, capped at maxItems, persisted. Returns the item count.
export async function fetchBulkItemsStep(runId: string): Promise<number> {
  "use step";
  const run = await getBulkRun(runId);
  if (!run) throw new Error(`bulk run not found: ${runId}`);

  const res = await executeComposioAction(run.tenantId, run.fetch.tool, run.fetch.args);
  if (!res.ok) {
    throw new Error(`fetch ${run.fetch.tool} failed: ${res.error ?? "unknown"}`);
  }
  const rows = normalizeRows(res.data, {
    itemsPath: run.fetch.itemsPath,
    headerRow: run.fetch.headerRow,
  });
  const capped = rows.slice(0, run.maxItems);
  await saveBulkItems(runId, capped);
  await patchBulkRun(runId, { total: capped.length, status: "running" });
  await recordActivity(run.tenantId, {
    kind: "tool",
    summary: `bulk ${runId}: fetched ${capped.length} rows via ${run.fetch.tool}${
      rows.length > capped.length ? ` (capped from ${rows.length})` : ""
    }`,
    meta: { runId, total: capped.length },
  });
  return capped.length;
}

// Per-item retry with exponential backoff. Only genuinely retryable failures
// get retried; auth/permission errors abort immediately (retrying a revoked
// token 3× per row × 1000 rows is pointless and slow).
const MAX_ATTEMPTS = 3;
const BACKOFF_MS = [1000, 4000];

function isRateLimitError(err: string): boolean {
  return /429|rate.?limit|too.?many.?requests|retry.?after/i.test(err);
}

// Returns the backoff delay in milliseconds for a given error and attempt index
// (0-based). Rate-limit errors get a 5 s minimum so: 5 s, 10 s. All other
// errors use the standard 1 s / 4 s ladder.
function retryDelayMs(err: string, attempt: number): number {
  if (isRateLimitError(err)) {
    return 5000 * (attempt + 1); // 5000, 10000, …
  }
  return BACKOFF_MS[attempt] ?? 4000; // 1000, 4000, …
}

function isTerminalToolError(err: string): boolean {
  return /401|403|not.?connected|no connected account|unauthoriz|permission|invalid.?grant/i.test(
    err
  );
}

export type BulkBatchResult = {
  processed: number; // items examined this batch (incl. skips/replays)
  done: number; // cumulative succeeded
  failed: number; // cumulative failed
  skipped: number; // cumulative skipped
  halted: boolean; // /stop or terminal auth error — stop the whole run
  haltReason?: string;
};

export async function processBulkBatchStep(args: {
  runId: string;
  start: number;
}): Promise<BulkBatchResult> {
  "use step";
  const run = await getBulkRun(args.runId);
  if (!run) throw new Error(`bulk run not found: ${args.runId}`);

  // /stop halts the run between batches — same gate the rest of chat uses.
  if (await isChatStopped(run.channel, run.sessionId)) {
    await patchBulkRun(args.runId, { status: "cancelled", finishedAt: Date.now() });
    return {
      processed: 0,
      done: run.done,
      failed: run.failed,
      skipped: run.skipped,
      halted: true,
      haltReason: "halted by /stop",
    };
  }

  const items = await loadBulkItems(args.runId);
  const end = Math.min(args.start + run.batchSize, items.length);
  let done = run.done;
  let failed = run.failed;
  let skipped = run.skipped;
  let halted = false;
  let haltReason: string | undefined;

  for (let idx = args.start; idx < end; idx++) {
    // Idempotency: never repeat an item that already succeeded (or was already
    // recorded as failed/skipped) — this is what makes step replays safe.
    const prior = await getItemStatus(args.runId, idx);
    if (prior) continue;

    const row = items[idx];

    // Resolve every arg template against this row.
    const resolved: Record<string, unknown> = {};
    const missing: string[] = [];
    for (const [k, tpl] of Object.entries(run.action.argsTemplate)) {
      const r = resolveRowTemplate(tpl, row);
      resolved[k] = r.value;
      missing.push(...r.missing);
    }
    // A row missing a referenced column, or resolving to an entirely empty
    // required-looking payload, is skipped — not failed — so trailing blank
    // sheet rows don't count as errors.
    const allEmpty = Object.values(resolved).every((v) => String(v).trim() === "");
    if (missing.length || allEmpty) {
      await setItemStatus(args.runId, idx, `skip:${missing.join(",") || "empty row"}`);
      skipped++;
      continue;
    }

    if (run.dryRun) {
      await setItemStatus(args.runId, idx, "ok:dry_run");
      done++;
      continue;
    }

    // Execute with bounded retries + backoff.
    let lastErr = "";
    let ok = false;
    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
      const res = await executeComposioAction(run.tenantId, run.action.tool, resolved);
      if (res.ok) {
        ok = true;
        break;
      }
      lastErr = res.error ?? "unknown error";
      if (isTerminalToolError(lastErr)) break; // auth is dead — don't grind retries
      if (attempt < MAX_ATTEMPTS - 1) {
        await new Promise((r) => setTimeout(r, retryDelayMs(lastErr, attempt)));
      }
    }

    if (ok) {
      await setItemStatus(args.runId, idx, "ok");
      done++;
    } else {
      await setItemStatus(args.runId, idx, `fail:${lastErr.slice(0, 160)}`);
      await recordBulkFailure(args.runId, idx, lastErr);
      failed++;
      if (isTerminalToolError(lastErr)) {
        halted = true;
        haltReason = `connection error on ${run.action.tool}: ${lastErr.slice(0, 160)}`;
        break;
      }
    }

    // Inter-item pacing (provider rate limits).
    if (run.itemDelayMs > 0 && idx < end - 1) {
      await new Promise((r) => setTimeout(r, run.itemDelayMs));
    }
  }

  await patchBulkRun(args.runId, { done, failed, skipped });
  return { processed: end - args.start, done, failed, skipped, halted, haltReason };
}

// Progress ping to the run's chat (called by the workflow at milestones).
export async function notifyBulkProgressStep(args: {
  runId: string;
  note: string;
}): Promise<void> {
  "use step";
  const run = await getBulkRun(args.runId);
  if (!run) return;
  try {
    await sendOutboundRuntime({
      channel: run.channel,
      sessionId: run.sessionId,
      text: args.note,
    });
  } catch {
    // progress pings are best-effort
  }
}

export async function finalizeBulkRunStep(args: {
  runId: string;
  halted?: boolean;
  haltReason?: string;
}): Promise<void> {
  "use step";
  const run = await getBulkRun(args.runId);
  if (!run) return;

  const status: BulkRun["status"] = args.halted
    ? args.haltReason === "halted by /stop"
      ? "cancelled"
      : "failed"
    : run.failed > 0 && run.done === 0
      ? "failed"
      : "done";
  await patchBulkRun(args.runId, {
    status,
    finishedAt: Date.now(),
    ...(args.haltReason ? { error: args.haltReason } : {}),
  });

  const failures = run.failed > 0 ? await listBulkFailures(args.runId, 5) : [];
  const lines = [
    status === "done"
      ? `✅ Bulk run finished: ${run.description}`
      : status === "cancelled"
        ? `🛑 Bulk run stopped: ${run.description}`
        : `⚠️ Bulk run ended with errors: ${run.description}`,
    `${run.dryRun ? "DRY RUN — nothing was actually sent. " : ""}${run.done}/${run.total} succeeded` +
      `${run.failed ? ` · ${run.failed} failed` : ""}${run.skipped ? ` · ${run.skipped} skipped (empty/missing fields)` : ""}.`,
    ...(args.haltReason && args.haltReason !== "halted by /stop"
      ? [`Stopped early: ${args.haltReason}`]
      : []),
    ...(failures.length
      ? [`Recent failures:\n${failures.map((f) => `  • ${f}`).join("\n")}`]
      : []),
    ...(run.failed > 0
      ? [`Ask me to retry ${run.id} — only the failed rows are re-attempted; succeeded rows are never repeated.`]
      : []),
  ];
  try {
    await sendOutboundRuntime({
      channel: run.channel,
      sessionId: run.sessionId,
      text: lines.join("\n"),
    });
  } catch {
    // summary delivery is best-effort; the run record holds the counts
  }
  await recordActivity(run.tenantId, {
    kind: "tool",
    summary: `bulk ${args.runId}: ${status} (${run.done}/${run.total} ok, ${run.failed} failed)`,
    meta: { runId: args.runId, status, done: run.done, failed: run.failed },
  });
}

export async function failBulkRunStep(args: { runId: string; error: string }): Promise<void> {
  "use step";
  const run = await patchBulkRun(args.runId, {
    status: "failed",
    error: args.error.slice(0, 400),
    finishedAt: Date.now(),
  });
  if (run) {
    try {
      await sendOutboundRuntime({
        channel: run.channel,
        sessionId: run.sessionId,
        text: `❌ Bulk run failed before completing: ${args.error.slice(0, 300)}\nAlready-sent items are recorded — re-running will NOT repeat them.`,
      });
    } catch {
      // best-effort
    }
  }
}
