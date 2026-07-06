// app/lib/evals/batchCompare.ts
//
// Batch-API variant of the model-compare harness. The expensive, latency-
// tolerant part of a compare run is the LLM JUDGE (a gpt-5.4 grade per cell).
// Those judge calls are identical-shape /v1/chat/completions requests with no
// tool loop, so they're a perfect fit for the OpenAI Batch API (50% cheaper,
// async). Everything else — running the candidate model and the deterministic
// code checks — still happens synchronously here, because candidate models can
// be non-OpenAI (Gemini / gateway) and code grades are free.
//
// Flow:
//   submitCompareBatch  → run every (model × task) cell, finalize judge-less
//                         cells immediately, collect the judge requests for the
//                         rest into ONE batch, persist a pending record.
//   pollCompareBatches  → (called each daemon tick) check pending batches; when
//                         one completes, merge judge outputs with the stored
//                         code grades and finalize those cells.
//
// Gated by EVALS_USE_BATCH at the call sites; this module is import-safe either
// way (no Node builtins — daemon-reachable).

import { getStore } from "@/app/lib/store";
import { env } from "@/app/lib/env";
import type { GraderResult, EvalRun } from "@/app/lib/evals/types";
import {
  COMPARE_TASKS,
  DEFAULT_COMPARE_MODELS,
  type TaskResult,
  ensureCompareCase,
  finalizeCell,
  getCompareTask,
  JUDGE_SYSTEM,
  judgeUserPrompt,
  judgeResultFromJson,
} from "@/app/lib/evals/modelCompare";
import {
  submitBatch,
  getBatchStatus,
  fetchBatchOutputs,
  chatCompletionText,
  isTerminalStatus,
  type BatchRequest,
} from "@/app/lib/openaiBatch";

const PENDING_SET = "nx_evals:batch:pending";
const batchKey = (id: string) => `nx_evals:batch:${id}`;

// Judge model for the batch path. MUST be an OpenAI model (Batch only hits
// OpenAI). gpt-5.4 mirrors the inline judge; never route this to gpt-5.2.
function judgeModel(): string {
  return env("EVALS_BATCH_JUDGE_MODEL") ?? "gpt-5.4";
}

// A cell whose judge grade is deferred to the batch. Stored in Redis so the
// poller can finalize it once the batch result lands.
type PendingCell = {
  customId: string;
  model: string;
  taskId: string;
  result: TaskResult;
  codeGrades: GraderResult[];
};

type PendingBatch = {
  batchId: string;
  createdAt: number;
  judgeModel: string;
  cells: PendingCell[];
};

function judgeRequestBody(rubric: string, output: string): Record<string, unknown> {
  return {
    model: judgeModel(),
    messages: [
      {
        role: "system",
        content:
          JUDGE_SYSTEM +
          ' Respond ONLY with a JSON object: {"score": <number 0-10>, "pass": <boolean>, "notes": <string>}.',
      },
      { role: "user", content: judgeUserPrompt(rubric, output) },
    ],
    response_format: { type: "json_object" },
  };
}

function judgeErrorGrade(reason: string): GraderResult {
  return { grader: "llm", name: "judge", pass: false, notes: `judge error: ${reason.slice(0, 200)}` };
}

export type SubmitCompareBatchResult = {
  batchId: string | null;
  finalizedNow: number;
  deferred: number;
  errors: number;
};

// Run all cells; finalize judge-less ones immediately, defer the rest to a
// single OpenAI batch. Returns the batch id (or null if no judge was needed).
export async function submitCompareBatch(opts: {
  models?: string[];
  taskIds?: string[];
} = {}): Promise<SubmitCompareBatchResult> {
  const models = opts.models?.length ? opts.models : DEFAULT_COMPARE_MODELS;
  const tasks = opts.taskIds?.length
    ? COMPARE_TASKS.filter((t) => opts.taskIds!.includes(t.id))
    : COMPARE_TASKS;

  const requests: BatchRequest[] = [];
  const pendingCells: PendingCell[] = [];
  let finalizedNow = 0;
  let errors = 0;
  let i = 0;

  for (const task of tasks) {
    await ensureCompareCase(task);
    for (const model of models) {
      let result: TaskResult;
      try {
        result = await task.run(model);
      } catch (err: any) {
        await finalizeCell({
          model,
          task,
          result: { finalText: "", toolCalls: [], durationMs: 0 },
          grades: [{ grader: "code", name: "executed", pass: false, notes: String(err?.message ?? err).slice(0, 300) }],
          errorMessage: String(err?.message ?? err).slice(0, 400),
        });
        errors++;
        continue;
      }

      const codeGrades = task.gradeCode(result);
      const spec = task.judgeSpec(result);
      if (!spec) {
        await finalizeCell({ model, task, result, grades: codeGrades });
        finalizedNow++;
        continue;
      }

      const customId = `cell_${i++}`;
      requests.push({
        custom_id: customId,
        method: "POST",
        url: "/v1/chat/completions",
        // Bound stored payload: finalText is re-sliced on finalize anyway.
        body: judgeRequestBody(spec.rubric, spec.output),
      });
      pendingCells.push({
        customId,
        model,
        taskId: task.id,
        result: { ...result, finalText: result.finalText.slice(0, 4000) },
        codeGrades,
      });
    }
  }

  if (requests.length === 0) {
    return { batchId: null, finalizedNow, deferred: 0, errors };
  }

  const batchId = await submitBatch(requests, {
    endpoint: "/v1/chat/completions",
    metadata: { kind: "model-compare" },
  });

  const record: PendingBatch = {
    batchId,
    createdAt: Date.now(),
    judgeModel: judgeModel(),
    cells: pendingCells,
  };
  const store = getStore();
  await store.set(batchKey(batchId), record);
  await store.sadd(PENDING_SET, batchId);

  return { batchId, finalizedNow, deferred: pendingCells.length, errors };
}

async function finalizePendingCell(
  cell: PendingCell,
  judge: GraderResult
): Promise<EvalRun | null> {
  const task = getCompareTask(cell.taskId);
  if (!task) return null;
  return finalizeCell({
    model: cell.model,
    task,
    result: cell.result,
    grades: [...cell.codeGrades, judge],
  });
}

export type PollCompareBatchesResult = {
  checked: number;
  completed: number;
  stillRunning: number;
  finalizedCells: number;
};

// Poll every pending compare batch; finalize the deferred cells of any that
// reached a terminal state. Safe to call on every daemon tick.
export async function pollCompareBatches(opts: { maxBatches?: number } = {}): Promise<PollCompareBatchesResult> {
  const store = getStore();
  const ids = await store.smembers(PENDING_SET);
  const out: PollCompareBatchesResult = { checked: 0, completed: 0, stillRunning: 0, finalizedCells: 0 };
  const cap = opts.maxBatches ?? 5;

  for (const batchId of ids.slice(0, cap)) {
    out.checked++;
    const record = await store.get<PendingBatch>(batchKey(batchId));
    if (!record) {
      await store.srem(PENDING_SET, batchId);
      continue;
    }

    let status;
    try {
      status = await getBatchStatus(batchId);
    } catch {
      out.stillRunning++;
      continue;
    }

    if (!isTerminalStatus(status.status)) {
      out.stillRunning++;
      continue;
    }

    // Terminal. Pull outputs (if any) and finalize every deferred cell.
    let outputs = new Map<string, { statusCode: number; body: any; error: any }>();
    if (status.status === "completed" && status.output_file_id) {
      try {
        outputs = await fetchBatchOutputs(status.output_file_id);
      } catch {
        /* fall through — cells get a judge-error grade */
      }
    }

    for (const cell of record.cells) {
      const o = outputs.get(cell.customId);
      let judge: GraderResult;
      if (o && o.statusCode >= 200 && o.statusCode < 300 && !o.error) {
        try {
          judge = judgeResultFromJson(JSON.parse(chatCompletionText(o.body)));
        } catch (err: any) {
          judge = judgeErrorGrade(`unparsable judge output: ${String(err?.message ?? err)}`);
        }
      } else {
        judge = judgeErrorGrade(
          o?.error ? JSON.stringify(o.error) : `batch ${status.status} (no output for ${cell.customId})`
        );
      }
      const run = await finalizePendingCell(cell, judge);
      if (run) out.finalizedCells++;
    }

    await store.srem(PENDING_SET, batchId);
    await store.del(batchKey(batchId));
    out.completed++;
  }

  return out;
}
