// app/api/ui/debug-manifest/route.ts
//
// Diagnostic endpoint: reports where the WDK manifest actually is on the
// deployed function so we can tell whether outputFileTracingIncludes did
// what we expected. UI-cookie-gated like every other /api/ui/* route.
//
// Drop this once the "No Workflows Found" issue is closed.

import { NextResponse } from "next/server";
import { promises as fs } from "node:fs";
import path from "node:path";

import { requireUiAuthPage } from "@/app/lib/uiRequire";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

const CANDIDATES = [
  // Whatever WORKFLOW_MANIFEST_PATH resolves to.
  process.env.WORKFLOW_MANIFEST_PATH ?? null,
  // The path the dashboard searches when WORKFLOW_MANIFEST_PATH is unset.
  path.join(process.cwd(), "app/.well-known/workflow/v1/manifest.json"),
  path.join(process.cwd(), "src/app/.well-known/workflow/v1/manifest.json"),
  // The public/ copy WORKFLOW_PUBLIC_MANIFEST writes.
  path.join(process.cwd(), "public/.well-known/workflow/v1/manifest.json"),
  // Vercel-conventional task root, in case cwd differs.
  "/var/task/app/.well-known/workflow/v1/manifest.json",
  "/var/task/public/.well-known/workflow/v1/manifest.json",
].filter((p): p is string => !!p);

async function describe(p: string) {
  try {
    const stat = await fs.stat(p);
    const head = await fs.readFile(p, "utf8");
    const parsed = JSON.parse(head);
    return {
      path: p,
      exists: true,
      size: stat.size,
      workflowCount: Object.keys(parsed.workflows ?? {}).length,
      workflowNames: Object.values<any>(parsed.workflows ?? {})
        .flatMap((g) => Object.keys(g ?? {}))
        .slice(0, 20),
    };
  } catch (e: any) {
    return {
      path: p,
      exists: false,
      error: String(e?.code ?? e?.message ?? e).slice(0, 200),
    };
  }
}

export async function GET() {
  await requireUiAuthPage();
  const results = await Promise.all(CANDIDATES.map(describe));
  return NextResponse.json({
    ok: true,
    cwd: process.cwd(),
    env: {
      WORKFLOW_MANIFEST_PATH: process.env.WORKFLOW_MANIFEST_PATH ?? null,
      WORKFLOW_PUBLIC_MANIFEST: process.env.WORKFLOW_PUBLIC_MANIFEST ?? null,
      WORKFLOW_LOCAL_DATA_DIR: process.env.WORKFLOW_LOCAL_DATA_DIR ?? null,
      WORKFLOW_EMBEDDED_DATA_DIR: process.env.WORKFLOW_EMBEDDED_DATA_DIR ?? null,
      VERCEL: process.env.VERCEL ?? null,
    },
    candidates: results,
  });
}
