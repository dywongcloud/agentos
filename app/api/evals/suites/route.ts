// app/api/evals/suites/route.ts
//
// Per-suite pass-rate summary across the last N runs (default 50).
// First place Claude should look when investigating "is the system
// healthier or worse than last week" — gives a one-shot snapshot
// across all eval suites without paging through raw runs.

import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";

import { listSuites, suiteSummary } from "@/app/lib/evals/store";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(req: NextRequest) {
  const url = new URL(req.url);
  const recentN = Math.max(1, Math.min(500, Number(url.searchParams.get("recentN") ?? 50)));
  const suites = await listSuites();
  const summaries = await Promise.all(suites.map((s) => suiteSummary(s, recentN)));
  // Surface lowest pass rate first — that's where attention is needed.
  summaries.sort((a, b) => a.passRate - b.passRate);
  return NextResponse.json({ ok: true, suites: summaries });
}
