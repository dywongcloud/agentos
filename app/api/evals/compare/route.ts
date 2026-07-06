// app/api/evals/compare/route.ts
//
// Head-to-head model comparison driver. One (model × task) cell per request
// so each call stays well inside the function duration limit; the scoreboard
// aggregates the newest cell per pair.
//
//   GET /api/evals/compare?model=claude-opus-4.8&task=mc_styled_doc   run one cell
//   GET /api/evals/compare?model=fable-5                              run all tasks for a model
//   GET /api/evals/compare?summary=1                                  scoreboard
//   GET /api/evals/compare?tasks=1                                    list task ids
//   GET /api/evals/compare?mode=batch&models=gpt-5.4,fable-5          submit a Batch-API run
//   GET /api/evals/compare?poll=1                                     finalize completed batches

import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";

import {
  COMPARE_TASKS,
  DEFAULT_COMPARE_MODELS,
  runCompareCell,
  compareSummary,
} from "@/app/lib/evals/modelCompare";
import { submitCompareBatch, pollCompareBatches } from "@/app/lib/evals/batchCompare";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export const maxDuration = 300;

export async function GET(req: NextRequest) {
  const q = new URL(req.url).searchParams;

  if (q.get("summary")) {
    return NextResponse.json({ ok: true, scoreboard: await compareSummary() });
  }
  if (q.get("tasks")) {
    return NextResponse.json({
      ok: true,
      tasks: COMPARE_TASKS.map((t) => ({ id: t.id, name: t.name })),
    });
  }

  // Manual poll: finalize any completed Batch-API runs now (the daemon does
  // this each tick; this is for on-demand checking).
  if (q.get("poll")) {
    return NextResponse.json({ ok: true, poll: await pollCompareBatches() });
  }

  // Batch mode: run generation + code grades synchronously, defer the LLM judge
  // to the OpenAI Batch API (50% cheaper, async). Daemon finalizes on completion.
  if (q.get("mode") === "batch" || q.get("batch")) {
    const models = (q.get("models") ?? q.get("model") ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const taskId = q.get("task");
    const res = await submitCompareBatch({
      models: models.length ? models : DEFAULT_COMPARE_MODELS,
      taskIds: taskId ? [taskId] : undefined,
    });
    return NextResponse.json({ ok: true, mode: "batch", ...res });
  }

  const model = q.get("model") ?? "";
  if (!model) {
    return NextResponse.json({ ok: false, error: "model is required (or pass summary=1 / tasks=1)" });
  }
  const taskId = q.get("task");
  const taskIds = taskId ? [taskId] : COMPARE_TASKS.map((t) => t.id);

  const cells = [];
  for (const id of taskIds) {
    try {
      const run = await runCompareCell({ model, taskId: id });
      cells.push({
        task: id,
        status: run.status,
        grades: run.grades.map((g) => ({ name: g.name, pass: g.pass, score: g.score, notes: g.notes })),
        durationMs: run.actual.durationMs,
        error: run.actual.errorMessage,
      });
    } catch (err: any) {
      cells.push({ task: id, status: "error", error: String(err?.message ?? err).slice(0, 300) });
    }
  }
  return NextResponse.json({ ok: true, model, cells });
}
