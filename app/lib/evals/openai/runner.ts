// app/lib/evals/openai/runner.ts
//
// Drives the OpenAI Evals API for each inference flow (see flows.ts) and
// folds the results back into our own Redis eval store (putRun) so they
// show up in /ui/evals + the activity feed alongside live-traffic evals.
//
// Two-phase so it fits inside a serverless function budget:
//   1. start  — create the OpenAI eval + run (responses data source) and
//      write a *pending* EvalRun (stable id, meta.pending=true). Cheap/fast.
//   2. reconcile — poll the OpenAI run; once terminal, pull output_items,
//      aggregate per-grader pass/score, and re-write the SAME EvalRun id with
//      the final status. Idempotent; safe to call repeatedly (e.g. from cron).
//
// Audio flows (voice/TTS) can't be sampled by the text Responses Evals API —
// they're recorded immediately as an honest "partial" info row.

import OpenAI from "openai";

import { env } from "@/app/lib/env";
import { resolveModelName, isReasoningModel } from "@/app/lib/modelRouting";
import { putRun, getRun, listRuns, deleteRun } from "@/app/lib/evals/store";
import type { EvalRun, GraderResult } from "@/app/lib/evals/types";
import {
  listFlows,
  getFlow,
  itemSchemaFor,
  type FlowEvalSpec,
} from "./flows";

export const OAI_EVAL_SUITE = "openai-evals";

function client(): OpenAI {
  return new OpenAI({ apiKey: env("OPENAI_API_KEY") });
}

function graderModel(): string {
  return env("OAI_EVAL_GRADER_MODEL") ?? resolveModelName("fast-meta");
}

function deployId(): string | undefined {
  return env("VERCEL_GIT_COMMIT_SHA") ?? env("VERCEL_DEPLOYMENT_ID") ?? undefined;
}

// Stable EvalRun id per flow per start, so reconcile can overwrite in place.
function recordId(flowId: string, startedAt: number): string {
  return `er_oai_${flowId}_${startedAt.toString(36)}`;
}

const GRADER_NAME = "openai_score_model";

function evalModelFor(flow: FlowEvalSpec): string {
  return resolveModelName(flow.evalPurpose);
}

// --- audio flows: honest info row, no live run --------------------------

async function recordAudioFlow(flow: FlowEvalSpec): Promise<EvalRun> {
  const startedAt = Date.now();
  return putRun({
    id: recordId(flow.id, startedAt),
    caseId: `oai_${flow.id}`,
    suite: OAI_EVAL_SUITE,
    ts: startedAt,
    deployId: deployId(),
    input: {
      goal: `OpenAI eval — ${flow.name}`,
      channel: "openai-evals",
      meta: {
        flow: flow.id,
        flowName: flow.name,
        blurb: flow.blurb,
        realProvider: flow.realProvider,
        trigger: flow.trigger,
        audio: true,
        pending: false,
      },
    },
    actual: {
      toolCalls: [],
      artifactPaths: [],
      finalText:
        "Audio modality (Whisper / TTS) — not evaluable via the text Responses Evals API. Surfaced for coverage.",
    },
    grades: [
      {
        grader: "code",
        name: "audio_not_text_evaluable",
        pass: true,
        notes: "Audio flow recorded for visibility; excluded from text grading.",
      },
    ],
    status: "partial",
  });
}

// --- phase 1: start -----------------------------------------------------

export type StartResult = {
  flowId: string;
  recordId: string;
  openaiEvalId?: string;
  openaiRunId?: string;
  evalModel?: string;
  audio?: boolean;
  error?: string;
};

export async function startFlowEval(flow: FlowEvalSpec): Promise<StartResult> {
  if (flow.audio) {
    const rec = await recordAudioFlow(flow);
    return { flowId: flow.id, recordId: rec.id, audio: true };
  }

  const startedAt = Date.now();
  const id = recordId(flow.id, startedAt);
  const model = evalModelFor(flow);
  const oai = client();

  try {
    // 1a. Register the eval (schema + grader).
    const ev = await oai.evals.create({
      name: `agentos:${flow.id}`,
      metadata: { flow: flow.id, suite: OAI_EVAL_SUITE },
      data_source_config: {
        type: "custom",
        item_schema: itemSchemaFor(flow) as Record<string, unknown>,
        include_sample_schema: true,
      },
      testing_criteria: [
        {
          type: "score_model",
          name: GRADER_NAME,
          model: graderModel(),
          input: [
            {
              role: "system",
              content:
                "You are a strict evaluation grader. Read the rubric and the " +
                "model output, then respond with ONLY a single integer from 1 to " +
                "7 (no words, no explanation). 1 is the worst, 7 is the best.",
            },
            { role: "user", content: flow.graderRubric },
          ],
          range: [1, 7],
          pass_threshold: 4.5,
        },
      ],
    } as any);

    // 1b. Launch a run that samples the flow's real model+prompt+schema.
    const reasoning = isReasoningModel(model);
    const samplingParams: Record<string, unknown> = {
      max_completions_tokens: 6000,
    };
    if (!reasoning) samplingParams.temperature = 0.2;
    if (flow.structured && flow.outputSchema) {
      samplingParams.text = {
        format: {
          type: "json_schema",
          name: flow.outputSchema.name,
          schema: flow.outputSchema.schema,
          strict: true,
        },
      };
    }

    const run = await oai.evals.runs.create(ev.id, {
      name: `${flow.id}-${startedAt}`,
      data_source: {
        type: "responses",
        source: {
          type: "file_content",
          content: flow.golden.map((item) => ({ item })),
        },
        model,
        input_messages: {
          type: "template",
          template: [
            { role: "system", content: flow.system },
            { role: "user", content: flow.userTemplate },
          ],
        },
        sampling_params: samplingParams,
      },
    } as any);

    const evalUrl = `https://platform.openai.com/evals/${ev.id}`;
    await putRun({
      id,
      caseId: `oai_${flow.id}`,
      suite: OAI_EVAL_SUITE,
      ts: startedAt,
      deployId: deployId(),
      input: {
        goal: `OpenAI eval — ${flow.name}`,
        channel: "openai-evals",
        meta: {
          flow: flow.id,
          flowName: flow.name,
          blurb: flow.blurb,
          realProvider: flow.realProvider,
          trigger: flow.trigger,
          structured: flow.structured,
          itemCount: flow.golden.length,
          evalModel: model,
          graderModel: graderModel(),
          openaiEvalId: ev.id,
          openaiRunId: run.id,
          evalUrl,
          providerSubstituted: flow.realProvider !== "openai",
          pending: true,
        },
      },
      actual: {
        toolCalls: [],
        artifactPaths: [],
        finalText: `Run launched on OpenAI (${model}). ${flow.golden.length} item(s); awaiting grading.`,
      },
      grades: [],
      status: "partial",
    });

    return {
      flowId: flow.id,
      recordId: id,
      openaiEvalId: ev.id,
      openaiRunId: run.id,
      evalModel: model,
    };
  } catch (err: any) {
    const msg = String(err?.message ?? err).slice(0, 400);
    await putRun({
      id,
      caseId: `oai_${flow.id}`,
      suite: OAI_EVAL_SUITE,
      ts: startedAt,
      deployId: deployId(),
      input: {
        goal: `OpenAI eval — ${flow.name}`,
        channel: "openai-evals",
        meta: {
          flow: flow.id,
          flowName: flow.name,
          realProvider: flow.realProvider,
          evalModel: model,
          pending: false,
        },
      },
      actual: {
        toolCalls: [],
        artifactPaths: [],
        errorMessage: msg,
        finalText: `Failed to launch OpenAI eval: ${msg}`,
      },
      grades: [
        { grader: "code", name: "launch", pass: false, notes: msg },
      ],
      status: "error",
    });
    return { flowId: flow.id, recordId: id, evalModel: model, error: msg };
  }
}

// --- debug: inspect a run's raw failure detail --------------------------

export async function debugFlow(flowId: string): Promise<any> {
  const runs = await listRuns({ suite: OAI_EVAL_SUITE, limit: 200 });
  const rec = runs.find((r) => String(r.input.meta?.flow ?? "") === flowId);
  if (!rec) return { error: `no record for flow ${flowId}` };
  const evalId = String(rec.input.meta?.openaiEvalId ?? "");
  const runId = String(rec.input.meta?.openaiRunId ?? "");
  if (!evalId || !runId) return { recordId: rec.id, error: "no openai ids on record", meta: rec.input.meta };
  const oai = client();
  let run: any = null;
  let runErr: string | undefined;
  try {
    run = await oai.evals.runs.retrieve(runId, { eval_id: evalId });
  } catch (e: any) {
    runErr = String(e?.message ?? e).slice(0, 600);
  }
  const items: any[] = [];
  let itemsErr: string | undefined;
  try {
    for await (const it of oai.evals.runs.outputItems.list(runId, { eval_id: evalId } as any)) {
      items.push(it);
      if (items.length >= 3) break;
    }
  } catch (e: any) {
    itemsErr = String(e?.message ?? e).slice(0, 600);
  }
  return {
    flowId,
    recordId: rec.id,
    evalId,
    runId,
    evalModel: rec.input.meta?.evalModel,
    graderModel: rec.input.meta?.graderModel,
    runErr,
    itemsErr,
    run: run
      ? {
          status: run.status,
          error: run.error,
          result_counts: run.result_counts,
          report_url: run.report_url,
          per_model_usage: run.per_model_usage,
          per_testing_criteria_results: run.per_testing_criteria_results,
        }
      : null,
    itemCount: items.length,
    items: items.map((it) => ({
      status: it.status,
      datasource_item: it.datasource_item,
      results: it.results,
      sampleError: it.sample?.error,
      sampleFinishReason: it.sample?.finish_reason,
      sampleOutputText:
        typeof it.sample?.output_text === "string"
          ? it.sample.output_text.slice(0, 600)
          : bestEffortSampleText(it.sample)?.slice(0, 600),
    })),
  };
}

// --- phase 2: reconcile -------------------------------------------------

function bestEffortSampleText(sample: any): string | undefined {
  if (!sample) return undefined;
  if (typeof sample.output_text === "string") return sample.output_text;
  const out = sample.output;
  if (Array.isArray(out)) {
    const parts = out
      .map((o: any) =>
        typeof o?.content === "string"
          ? o.content
          : Array.isArray(o?.content)
            ? o.content.map((c: any) => c?.text ?? "").join("")
            : ""
      )
      .filter(Boolean);
    if (parts.length) return parts.join("\n");
  }
  try {
    return JSON.stringify(sample).slice(0, 1200);
  } catch {
    return undefined;
  }
}

export type ReconcileResult = {
  recordId: string;
  flowId: string;
  status: EvalRun["status"] | "still_running";
};

// Reconcile a single pending EvalRun. Returns its (possibly unchanged) state.
export async function reconcileRecord(rec: EvalRun): Promise<ReconcileResult> {
  const flowId = String(rec.input.meta?.flow ?? "");
  const evalId = String(rec.input.meta?.openaiEvalId ?? "");
  const runId = String(rec.input.meta?.openaiRunId ?? "");
  if (!evalId || !runId) {
    return { recordId: rec.id, flowId, status: rec.status };
  }
  const oai = client();

  let run: any;
  try {
    run = await oai.evals.runs.retrieve(runId, { eval_id: evalId });
  } catch (err: any) {
    return { recordId: rec.id, flowId, status: "still_running" };
  }

  const terminal = ["completed", "failed", "canceled"].includes(run.status);
  if (!terminal) {
    return { recordId: rec.id, flowId, status: "still_running" };
  }

  // Pull graded output items.
  const items: any[] = [];
  try {
    for await (const it of oai.evals.runs.outputItems.list(runId, {
      eval_id: evalId,
    } as any)) {
      items.push(it);
    }
  } catch {
    // fall through; we may still finalize from result_counts
  }

  // The Evals API appends the testing-criteria id to the grader name
  // (e.g. "openai_score_model-8fb62872-..."), so match by prefix and fall
  // back to the sole result when there's exactly one criterion.
  const resultsForGrader = items
    .map((it) => {
      const rs: any[] = it.results ?? [];
      return (
        rs.find((r: any) => typeof r.name === "string" && r.name.startsWith(GRADER_NAME)) ??
        (rs.length === 1 ? rs[0] : undefined)
      );
    })
    .filter(Boolean);
  const total = resultsForGrader.length;
  const passedN = resultsForGrader.filter((r: any) => r.passed).length;
  const avgScore =
    total > 0
      ? resultsForGrader.reduce((a: number, r: any) => a + (r.score ?? 0), 0) /
        total
      : 0;
  const passRate = total > 0 ? passedN / total : 0;

  const grades: GraderResult[] = [
    {
      grader: "llm",
      name: GRADER_NAME,
      pass: total > 0 && passedN === total,
      score: passRate,
      notes:
        total > 0
          ? `${passedN}/${total} item(s) passed; avg score ${avgScore.toFixed(2)}/7`
          : "no graded items returned",
    },
  ];

  let status: EvalRun["status"];
  if (run.status !== "completed" || total === 0) status = "error";
  else if (passedN === total) status = "pass";
  else if (passedN === 0) status = "fail";
  else status = "partial";

  const sampleText = bestEffortSampleText(items[0]?.sample);
  const counts = run.result_counts ?? {};

  await putRun({
    id: rec.id,
    caseId: rec.caseId,
    suite: rec.suite,
    ts: rec.ts,
    deployId: rec.deployId,
    input: {
      ...rec.input,
      meta: {
        ...(rec.input.meta ?? {}),
        pending: false,
        openaiRunStatus: run.status,
        resultCounts: counts,
        passRate,
        avgScore,
      },
    },
    actual: {
      toolCalls: [],
      artifactPaths: [],
      finalText:
        status === "error"
          ? `OpenAI run ${run.status}. ${
              counts.errored ? `${counts.errored} errored item(s).` : ""
            } ${sampleText ? `Sample: ${sampleText.slice(0, 400)}` : ""}`.trim()
          : `${passedN}/${total} item(s) passed (avg ${avgScore.toFixed(
              2
            )}/7).${sampleText ? `\n\nSample output:\n${sampleText.slice(0, 800)}` : ""}`,
      errorMessage: status === "error" ? `OpenAI run status: ${run.status}` : undefined,
    },
    grades,
    status,
  });

  return { recordId: rec.id, flowId, status };
}

// Find pending OpenAI-eval records and reconcile each. With force=true,
// re-reconciles every record that has an OpenAI run id (used to re-grade
// runs that were finalized under an older, buggy reconcile).
export async function reconcilePending(force = false): Promise<ReconcileResult[]> {
  const runs = await listRuns({ suite: OAI_EVAL_SUITE, limit: 200 });
  const targets = runs.filter((r) =>
    force ? !!r.input.meta?.openaiRunId : r.input.meta?.pending === true
  );
  const out: ReconcileResult[] = [];
  for (const r of targets) {
    out.push(await reconcileRecord(r));
  }
  return out;
}

// Purge stale launch-error rows: openai-evals records that errored before a
// run was ever created (no openaiRunId), i.e. noise from a bad launch param.
export async function purgeLaunchErrors(): Promise<{ deleted: string[] }> {
  const runs = await listRuns({ suite: OAI_EVAL_SUITE, limit: 200 });
  const deleted: string[] = [];
  for (const r of runs) {
    if (r.status === "error" && !r.input.meta?.openaiRunId) {
      if (await deleteRun(r.id)) deleted.push(r.id);
    }
  }
  return { deleted };
}

// --- orchestration ------------------------------------------------------

export async function runAllFlows(): Promise<StartResult[]> {
  const out: StartResult[] = [];
  for (const flow of listFlows()) {
    out.push(await startFlowEval(flow));
  }
  return out;
}

export async function runOneFlow(flowId: string): Promise<StartResult | null> {
  const flow = getFlow(flowId);
  if (!flow) return null;
  return startFlowEval(flow);
}

// Convenience for a synchronous "run + wait" of a single flow (used by the
// smoke path). Polls up to ~maxWaitMs for the run to finish.
export async function runFlowAndWait(
  flowId: string,
  maxWaitMs = 90_000
): Promise<{ start: StartResult | null; final?: ReconcileResult }> {
  const start = await runOneFlow(flowId);
  if (!start || start.audio || start.error) return { start };
  const deadline = Date.now() + maxWaitMs;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 4000));
    const rec = await getRun(start.recordId);
    if (!rec) continue;
    const res = await reconcileRecord(rec);
    if (res.status !== "still_running") return { start, final: res };
  }
  return { start };
}
