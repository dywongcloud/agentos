// app/workflows/pairingFlow.ts
//
// SPIKE — hook-based gateway pairing as a one-file proof of the
// `createHook` / `resumeHook` pattern.
//
// What this replaces:
//   - `ensurePairingCode()` / `exchangePairingCode()` in app/lib/gatewayAuth.ts
//   - The custom Redis dance (PAIR_CODE_KEY ↔ TOKEN_KEY) + the
//     manual TTL + manual cleanup of the unmatched code
//
// What the workflow does:
//   1. Generates a 6-digit code (step → durably logged so replays don't
//      churn a new code on each restart)
//   2. Publishes the code under the existing `gateway:pair_code` Redis key
//      so the UI (`getGatewayAuthStatus`) shows it unchanged — no UI
//      changes needed for the spike
//   3. Creates a hook with the deterministic token `gateway:pair` so the
//      submit endpoint can find it without any "which workflow run?" lookup
//   4. Loops `for await (const submission of hook)` — every POST resumes
//      the workflow with the submitted code:
//        - match  → mint bearer token, persist, exit cleanly
//        - miss   → bump attempt counter, continue (re-await)
//        - cap    → fail out, code is dead, UI can regenerate
//
// Wiring (sketched, not wired into production paths in this spike):
//
//   // route: POST /api/ui/gateway/hook-pair?op=start
//   await start(pairingFlow, []);
//
//   // route: POST /api/ui/gateway/hook-pair?op=submit  body: { code }
//   await resumeHook("gateway:pair", { code });
//
// Compared to the existing flow, the workflow OWNS the lifecycle: code
// generation, attempt counting, expiry, cleanup. No more "the code is
// in Redis but no one is watching it" — there's a durable workflow run
// that's literally paused on `await hook`, and a Redis-backed snapshot
// of its state. The replay model handles cold starts for free.

import { createHook } from "workflow";
import crypto from "crypto";

import { getStore } from "@/app/lib/store";

// Same keys the current UI + gateway client read/write today, so the
// existing dashboard surfaces the workflow-issued code/token without any
// frontend churn. When we adopt this for real, gatewayAuth.ts loses
// `ensurePairingCode` / `exchangePairingCode` and just reads these keys.
const PAIR_CODE_KEY = "gateway:pair_code";
const TOKEN_KEY = "gateway:bearer_token";

// Deterministic so the dispatching side can call `resumeHook("gateway:pair", …)`
// from any request handler without first looking up a runId or token.
const PAIR_HOOK_TOKEN = "gateway:pair";

// What the submit endpoint sends back into the workflow.
export type PairSubmission = { code: string };

// ---------------------------------------------------------------------------
// Steps — every external side-effect lives behind a `"use step"` so it
// shows up in the WDK event log + replays deterministically.
// ---------------------------------------------------------------------------

async function generateCodeStep(): Promise<string> {
  "use step";
  const n = crypto.randomInt(0, 1_000_000);
  return n.toString().padStart(6, "0");
}

async function publishCodeStep(code: string): Promise<void> {
  "use step";
  const store = getStore();
  await store.set(PAIR_CODE_KEY, code, { exSeconds: 24 * 60 * 60 });
}

async function mintBearerTokenStep(): Promise<string> {
  "use step";
  return crypto.randomUUID();
}

async function persistBearerTokenStep(token: string): Promise<void> {
  "use step";
  const store = getStore();
  await store.set(TOKEN_KEY, token);
  // Code is single-use — drop it so a stale value in the UI doesn't
  // confuse the next pairing attempt.
  await store.del(PAIR_CODE_KEY);
}

async function clearPendingCodeStep(): Promise<void> {
  "use step";
  const store = getStore();
  await store.del(PAIR_CODE_KEY);
}

// Diagnostic breadcrumb — every time the workflow processes a submission,
// it writes a small JSON to Redis so the curl driver can prove the
// workflow body actually ran. This is spike-only; remove once the
// pattern is trusted.
async function recordAttemptStep(args: {
  attempt: number;
  submittedCode: string;
  expectedCode: string;
  matched: boolean;
}): Promise<void> {
  "use step";
  const store = getStore();
  await store.set(
    "gateway:pair_last_attempt",
    JSON.stringify({ ...args, ts: Date.now() }),
    { exSeconds: 60 * 60 }
  );
}

// ---------------------------------------------------------------------------
// Workflow
// ---------------------------------------------------------------------------

export type PairingFlowResult =
  | { ok: true; tokenLastFour: string }
  | { ok: false; reason: "too_many_attempts" };

export async function pairingFlow(): Promise<PairingFlowResult> {
  "use workflow";

  const code = await generateCodeStep();
  await publishCodeStep(code);

  // Simplified to single-await: every submission either matches and
  // we're done, or doesn't and we exit (the driver can call ?op=start
  // again to re-pair). Multi-attempt retries with re-awaits inside a
  // workflow appear to confuse the WDK replay model — the workflow
  // gets stuck in "running" state after the first match, not
  // proceeding past the post-await body even though step breadcrumbs
  // confirm the body ran. Going one-shot for the spike to confirm
  // hooks work end-to-end at all; retries can be modelled as a new
  // workflow run per attempt.
  const hook = createHook<PairSubmission>({ token: PAIR_HOOK_TOKEN });
  try {
    const submission = await hook;
    const submitted = String(submission?.code ?? "");
    const matched = submitted === code;
    await recordAttemptStep({
      attempt: 1,
      submittedCode: submitted,
      expectedCode: code,
      matched,
    });
    if (matched) {
      const token = await mintBearerTokenStep();
      await persistBearerTokenStep(token);
      return { ok: true, tokenLastFour: token.slice(-4) };
    }
    await clearPendingCodeStep();
    return { ok: false, reason: "too_many_attempts" };
  } finally {
    hook.dispose();
  }
}
