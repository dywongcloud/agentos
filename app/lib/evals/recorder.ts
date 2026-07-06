// app/lib/evals/recorder.ts
//
// High-level entry point for the eval system. The /job lifecycle
// (finalizeJobStep on success, failJobStep on failure) calls
// `recordJobEval()` with the run's actual outputs. The recorder:
//
//   1. Seeds the registered cases if they aren't in Redis yet.
//   2. Matches the run against case triggers (goal keyword + modality).
//      Any number of cases may match.
//   3. If nothing matched, still logs a single "auto" run with default
//      lightweight graders so we accumulate a baseline corpus. v0's
//      flywheel argument: every real interaction is data, don't drop it.
//   4. Runs all graders for each matched case and persists one EvalRun
//      per match. Status aggregates from grader results.
//
// All failure modes are swallowed — eval recording must never break a
// real /job. The recorder's own errors get written as a synthetic
// EvalRun with status="error" so we still see them in the dashboard.

import { env } from "@/app/lib/env";

import {
  matchSeedCases,
  seedEvalCasesIfNeeded,
} from "./cases";
import { aggregateStatus, runGraders } from "./graders";
import { putRun } from "./store";
import type { GraderConfig, RunActual, RunInput } from "./types";

function currentDeployId(): string | undefined {
  return (
    env("VERCEL_GIT_COMMIT_SHA") ??
    env("VERCEL_DEPLOYMENT_ID") ??
    env("VERCEL_URL") ??
    undefined
  );
}

// Default graders applied when no seed case matches. Cheap, universal
// signals — every /job has these regardless of domain.
const DEFAULT_AUTO_GRADERS: GraderConfig[] = [
  { type: "code", name: "no_max_retries_exceeded" },
  { type: "code", name: "duration_under_ms", args: { ms: 20 * 60 * 1000 } },
  { type: "code", name: "cost_under", args: { usd: 5.0 } },
];

export type RecordJobEvalArgs = {
  jobId: string;
  input: RunInput;
  actual: RunActual;
};

export async function recordJobEval(args: RecordJobEvalArgs): Promise<void> {
  try {
    await seedEvalCasesIfNeeded();
    const deployId = currentDeployId();
    const matched = matchSeedCases(args.input);

    if (matched.length === 0) {
      const grades = await runGraders(
        DEFAULT_AUTO_GRADERS,
        args.actual,
        args.input
      );
      await putRun({
        caseId: "ec_auto",
        suite: "auto",
        deployId,
        input: args.input,
        actual: args.actual,
        grades,
        status: aggregateStatus(grades),
        jobId: args.jobId,
      });
      return;
    }

    for (const c of matched) {
      const grades = await runGraders(c.graders, args.actual, args.input);
      await putRun({
        caseId: c.id,
        suite: c.suite,
        deployId,
        input: args.input,
        actual: args.actual,
        grades,
        status: aggregateStatus(grades),
        jobId: args.jobId,
      });
    }
  } catch (err: any) {
    // Recorder itself failed — log a synthetic error run so the
    // dashboard surfaces the breakage instead of silently dropping it.
    try {
      await putRun({
        caseId: "ec_recorder_error",
        suite: "_meta",
        deployId: currentDeployId(),
        input: args.input,
        actual: args.actual,
        grades: [
          {
            grader: "code",
            name: "recorder_self_check",
            pass: false,
            notes: `recordJobEval threw: ${String(err?.message ?? err).slice(0, 200)}`,
          },
        ],
        status: "error",
        jobId: args.jobId,
      });
    } catch {
      // give up silently
    }
  }
}
