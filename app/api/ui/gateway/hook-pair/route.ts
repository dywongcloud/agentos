// app/api/ui/gateway/hook-pair/route.ts
//
// SPIKE driver for the hook-based pairing workflow in
// app/workflows/pairingFlow.ts. Two ops:
//
//   POST /api/ui/gateway/hook-pair?op=start
//     Starts the pairing workflow (if not already running). The code
//     becomes visible at the existing `gateway:pair_code` Redis key,
//     which the UI already reads via getGatewayAuthStatus.
//
//   POST /api/ui/gateway/hook-pair?op=submit   body: { code: "123456" }
//     Calls `resumeHook("gateway:pair", { code })`, which wakes the
//     paused workflow with the submitted code. If it matches, the
//     workflow mints + persists a bearer token and exits; if it
//     misses, the workflow stays paused and waits for the next submit.
//
// Auth-gated by the dashboard cookie. Production wiring would replace
// the existing `op=pair` branch in app/api/claw/route.ts with the
// same `resumeHook` call.

import { NextResponse } from "next/server";
import { start } from "workflow/api";
import {
  resumeHook,
  getHookByToken,
  cancelRun,
  getRun,
  getWorld,
} from "@workflow/core/runtime";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { getStore } from "@/app/lib/store";
import { pairingFlow } from "@/app/workflows/pairingFlow";

const PAIR_HOOK_TOKEN = "gateway:pair";
const PAIR_CODE_KEY = "gateway:pair_code";
const TOKEN_KEY = "gateway:bearer_token";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  await requireUiAuthPage();

  const url = new URL(req.url);
  const op = url.searchParams.get("op");

  if (op === "start") {
    // Deterministic hook tokens are "one workflow per token, forever" —
    // any prior workflow run that registered `gateway:pair` is still
    // holding the token even if its function instance has long since
    // died and its `code` variable is stale. New `start()` calls would
    // collide inside `createHook()` and crash silently, leaving the
    // zombie run as the resumeHook recipient. Find + cancel the prior
    // holder before starting a fresh one.
    let priorRunId: string | null = null;
    try {
      const existing = await getHookByToken(PAIR_HOOK_TOKEN);
      if (existing?.runId) {
        priorRunId = existing.runId;
        await cancelRun(getWorld(), existing.runId);
      }
    } catch {
      // no prior holder — fine
    }

    // Belt-and-suspenders: a residual bearer token from a previous
    // pairing would make the dashboard show `paired:true` even before
    // this new workflow finishes, which is confusing during testing.
    // `start` means "I want to pair again" — wipe both Redis keys.
    const store = getStore();
    await store.del(TOKEN_KEY);
    await store.del(PAIR_CODE_KEY);
    await store.del("gateway:pair_last_attempt");

    try {
      const run = await start(pairingFlow, []);
      return NextResponse.json({
        ok: true,
        started: true,
        runId: run?.runId,
        cancelledPrior: priorRunId,
      });
    } catch (e: any) {
      return NextResponse.json(
        { ok: false, error: String(e?.message ?? e) },
        { status: 500 }
      );
    }
  }

  if (op === "submit") {
    const body = await req.json().catch(() => null);
    const code = String(body?.code ?? "").trim();
    if (!/^\d{6}$/.test(code)) {
      return NextResponse.json(
        { ok: false, error: "code must be a 6-digit string" },
        { status: 400 }
      );
    }
    try {
      const hook = await resumeHook(PAIR_HOOK_TOKEN, { code });
      // The hook is just an ack that the payload was delivered. The
      // workflow itself decides match/miss; the caller polls the
      // existing gateway status endpoint to see whether the token
      // appeared.
      return NextResponse.json({ ok: true, runId: hook?.runId });
    } catch (e: any) {
      return NextResponse.json(
        { ok: false, error: String(e?.message ?? e) },
        { status: 404 }
      );
    }
  }

  return NextResponse.json(
    { ok: false, error: "unknown op; use ?op=start, ?op=status, or ?op=submit" },
    { status: 400 }
  );
}

// Quick read-only peek so the spike can be driven entirely from curl —
// no need to load the dashboard to see the published code.
export async function GET(req: Request) {
  await requireUiAuthPage();
  const url = new URL(req.url);
  if (url.searchParams.get("op") !== "status") {
    return NextResponse.json(
      { ok: false, error: "unknown op; use ?op=status" },
      { status: 400 }
    );
  }
  const store = getStore();
  const [code, token, lastAttemptRaw] = await Promise.all([
    store.get<string>(PAIR_CODE_KEY),
    store.get<string>(TOKEN_KEY),
    store.get<string>("gateway:pair_last_attempt"),
  ]);
  let lastAttempt: unknown = null;
  if (lastAttemptRaw) {
    try {
      lastAttempt = typeof lastAttemptRaw === "string"
        ? JSON.parse(lastAttemptRaw)
        : lastAttemptRaw;
    } catch {
      // ignore
    }
  }

  // Look up the workflow run holding our deterministic hook token.
  // `hookReady` lets the caller distinguish "the workflow exists but
  // hasn't reached createHook yet" from "no workflow at all".
  let hookRunId: string | null = null;
  let hookReady = false;
  try {
    const hook = await getHookByToken(PAIR_HOOK_TOKEN);
    hookRunId = hook?.runId ?? null;
    hookReady = !!hook;
  } catch {
    // no hook registered (yet)
  }

  let runStatus: string | null = null;
  let runReturn: unknown = null;
  let runCompletedAt: string | null = null;
  // If the workflow already finished, getHookByToken won't find it
  // (the hook is gone). Also look up the last-known runId via the
  // breadcrumb (lastAttempt was written by the run we care about).
  const lastAttemptRunHint =
    (lastAttempt as any)?.runId ?? hookRunId ?? null;
  if (lastAttemptRunHint || hookRunId) {
    const runId = hookRunId ?? lastAttemptRunHint;
    try {
      const run = getRun(runId);
      runStatus = (await run.status) ?? null;
      runCompletedAt = (await run.completedAt)?.toISOString() ?? null;
      // returnValue throws if the run isn't completed yet — guard it.
      if (runStatus === "completed" || runStatus === "failed") {
        try {
          runReturn = await run.returnValue;
        } catch (e: any) {
          runReturn = { error: String(e?.message ?? e) };
        }
      }
    } catch {
      // ignore
    }
  }

  return NextResponse.json({
    ok: true,
    paired: !!token,
    code: code ?? null,
    tokenLastFour: token ? token.slice(-4) : null,
    hookReady,
    hookRunId,
    runStatus,
    runCompletedAt,
    runReturn,
    lastAttempt,
  });
}
