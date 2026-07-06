// app/api/evals/route.ts
//
// Read-only listing endpoint for eval runs. Designed for grep-friendly
// JSON shape so Claude can curl it during future investigations:
//
//   curl -s "$BASE/api/evals?suite=external-data-fetch&limit=10"   list
//   curl -s "$BASE/api/evals?status=fail&limit=20"                 recent fails
//   curl -s "$BASE/api/evals?caseId=ec_seed_gmail_fetch_week"      one case
//
// Filters: `suite`, `caseId`, `deployId`, `status`, `limit`, `sinceMs`.
// All optional. With no filter, returns the most recent runs across
// the default "auto" suite (where unmatched live /jobs land).

import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";

import { listRuns } from "@/app/lib/evals/store";
import type { EvalRun } from "@/app/lib/evals/types";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

function parseStatus(v: string | null): EvalRun["status"] | undefined {
  if (v === "pass" || v === "fail" || v === "partial" || v === "error") return v;
  return undefined;
}

export async function GET(req: NextRequest) {
  const url = new URL(req.url);
  const q = url.searchParams;
  const runs = await listRuns({
    suite: q.get("suite") ?? undefined,
    caseId: q.get("caseId") ?? undefined,
    deployId: q.get("deployId") ?? undefined,
    status: parseStatus(q.get("status")),
    limit: q.get("limit") ? Math.max(1, Math.min(500, Number(q.get("limit")))) : 50,
    sinceMs: q.get("sinceMs") ? Number(q.get("sinceMs")) : undefined,
  });
  return NextResponse.json({ ok: true, count: runs.length, runs });
}
