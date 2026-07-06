// app/api/evals/[id]/route.ts
//
// Single-eval-run detail endpoint. Returns the full EvalRun JSON
// (input snapshot, actual output, every grader's pass/fail/notes).

import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";

import { getRun } from "@/app/lib/evals/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(
  _req: NextRequest,
  ctx: { params: Promise<{ id: string }> }
) {
  const { id } = await ctx.params;
  const run = await getRun(id);
  if (!run) {
    return NextResponse.json({ ok: false, error: "not found" }, { status: 404 });
  }
  return NextResponse.json({ ok: true, run });
}
