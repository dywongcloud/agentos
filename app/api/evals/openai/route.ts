// app/api/evals/openai/route.ts
//
// Trigger + reconcile endpoint for the OpenAI Evals harness (one faithful
// re-run per inference flow; see app/lib/evals/openai/flows.ts).
//
//   POST /api/evals/openai?op=run_all          create + launch all flows
//   POST /api/evals/openai?op=run&flow=<id>    launch one flow
//   POST /api/evals/openai?op=run_wait&flow=.  launch one flow + wait for grade
//   POST /api/evals/openai?op=reconcile        poll pending runs → finalize
//   GET  /api/evals/openai?op=list             list flows + recent run states
//
// These call the OpenAI API and run models — real spend — so the mutating
// ops are POST. Results land in the nx_evals store (suite "openai-evals")
// and surface in /ui/evals + the activity feed.

import { NextResponse } from "next/server";

import { listFlows } from "@/app/lib/evals/openai/flows";
import {
  runAllFlows,
  runOneFlow,
  runFlowAndWait,
  reconcilePending,
  purgeLaunchErrors,
  debugFlow,
  OAI_EVAL_SUITE,
} from "@/app/lib/evals/openai/runner";
import { listRuns } from "@/app/lib/evals/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";
export const maxDuration = 300;

export async function GET(req: Request) {
  const url = new URL(req.url);
  const op = url.searchParams.get("op") ?? "list";
  if (op === "list") {
    const runs = await listRuns({ suite: OAI_EVAL_SUITE, limit: 200 });
    const byFlow = new Map<string, (typeof runs)[number]>();
    for (const r of runs) {
      const f = String(r.input.meta?.flow ?? "");
      if (f && !byFlow.has(f)) byFlow.set(f, r); // newest first (listRuns is sorted)
    }
    const flows = listFlows().map((f) => {
      const r = byFlow.get(f.id);
      return {
        id: f.id,
        name: f.name,
        realProvider: f.realProvider,
        structured: f.structured,
        audio: !!f.audio,
        lastRun: r
          ? {
              runId: r.id,
              ts: r.ts,
              status: r.status,
              pending: r.input.meta?.pending === true,
              passRate: r.input.meta?.passRate,
              openaiEvalId: r.input.meta?.openaiEvalId,
              openaiRunId: r.input.meta?.openaiRunId,
            }
          : null,
      };
    });
    return NextResponse.json({ ok: true, suite: OAI_EVAL_SUITE, flows });
  }
  if (op === "debug") {
    const flow = url.searchParams.get("flow") ?? "";
    if (!flow) return NextResponse.json({ ok: false, error: "missing flow" }, { status: 400 });
    const detail = await debugFlow(flow);
    return NextResponse.json({ ok: true, op, detail });
  }
  return NextResponse.json({ ok: false, error: `unknown GET op: ${op}` }, { status: 400 });
}

export async function POST(req: Request) {
  const url = new URL(req.url);
  const op = url.searchParams.get("op") ?? "";
  const flow = url.searchParams.get("flow") ?? "";

  try {
    if (op === "run_all") {
      const results = await runAllFlows();
      return NextResponse.json({ ok: true, op, count: results.length, results });
    }
    if (op === "run") {
      if (!flow) return NextResponse.json({ ok: false, error: "missing flow" }, { status: 400 });
      const result = await runOneFlow(flow);
      if (!result) return NextResponse.json({ ok: false, error: `unknown flow: ${flow}` }, { status: 404 });
      return NextResponse.json({ ok: true, op, result });
    }
    if (op === "run_wait") {
      if (!flow) return NextResponse.json({ ok: false, error: "missing flow" }, { status: 400 });
      const result = await runFlowAndWait(flow);
      return NextResponse.json({ ok: true, op, result });
    }
    if (op === "reconcile") {
      const force = url.searchParams.get("force") === "1";
      const results = await reconcilePending(force);
      return NextResponse.json({ ok: true, op, count: results.length, results });
    }
    if (op === "purge_errors") {
      const { deleted } = await purgeLaunchErrors();
      return NextResponse.json({ ok: true, op, count: deleted.length, deleted });
    }
    return NextResponse.json({ ok: false, error: `unknown op: ${op}` }, { status: 400 });
  } catch (err: any) {
    return NextResponse.json(
      { ok: false, op, error: String(err?.message ?? err).slice(0, 500) },
      { status: 500 }
    );
  }
}
