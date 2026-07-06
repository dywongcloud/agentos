// app/lib/evals/types.ts
//
// Shared types for the agentOS eval system. The system has three core
// records — Case (the test definition), Grader (the assertion shape),
// and Run (a single execution against a Case). Storage layout uses Redis
// as a relational store with secondary sorted-set indexes; see store.ts.

export type GraderType = "code" | "llm";

// A grader is a single assertion against a Run's actual output. Multiple
// graders can run against a single Run; the Run's overall status is
// pass iff every grader passes, partial iff some pass, fail iff none do
// (or if the execution itself errored).
export type GraderConfig =
  | { type: "code"; name: CodeGraderName; args?: Record<string, unknown> }
  | { type: "llm"; name: string; rubric: string; passThreshold?: number };

export type CodeGraderName =
  | "tool_called"
  | "tool_called_with_args"
  | "output_includes"
  | "output_matches"
  | "cost_under"
  | "duration_under_ms"
  | "vfs_path_created"
  | "artifact_count_at_least"
  | "no_max_retries_exceeded";

export type GraderResult = {
  grader: GraderType;
  name: string;
  pass: boolean;
  // Optional 0..1 score for graders that produce continuous values
  // (LLM judges return this; most code graders just pass/fail).
  score?: number;
  notes?: string;
};

// Snapshot of what the system was given (inputs) for the eval Run.
// Kept small and self-contained so a future reader can understand the
// case without joining other tables.
export type RunInput = {
  goal: string;
  modality?: string;
  // Channel: where the request came from (telegram, ui, api). Useful for
  // filtering "only telegram /job evals" without scanning all runs.
  channel?: string;
  // Free-form metadata — feature flags, A/B groups, etc.
  meta?: Record<string, unknown>;
};

// Snapshot of what the system actually produced. The shape is kept flat
// so a future reader can grep + understand without unpacking nested
// JSON. Tool calls are stored as `<name>(<args-json-or-summary>)` strings
// for grep-friendly inspection.
export type RunActual = {
  finalText?: string;
  toolCalls: string[];
  artifactPaths: string[];
  costUsd?: number;
  durationMs?: number;
  iterations?: number;
  // The terminal status of the underlying /job (executing/completed/failed).
  // Distinct from the eval's pass/fail — a /job can complete but fail the
  // grader (e.g. completed without ever calling Gmail when asked to).
  jobStatus?: string;
  // Last error if the job failed.
  errorMessage?: string;
};

export type EvalRun = {
  id: string;
  caseId: string;
  // Logical group of evals. Cases without a registered seed get the
  // suite "auto" — distinguishes hand-curated regression cases from
  // organically-captured live-traffic samples.
  suite: string;
  ts: number;
  // Vercel git commit SHA at the time of the run. Lets you find "all
  // evals from this deploy" or compare run quality across deploys.
  deployId?: string;
  input: RunInput;
  actual: RunActual;
  grades: GraderResult[];
  status: "pass" | "fail" | "partial" | "error";
  // /job id for joining to the run's thought trail / cost ledger.
  jobId?: string;
};

export type EvalCase = {
  id: string;
  suite: string;
  name: string;
  // Triggers: which conditions on a live /job make this case
  // applicable. If empty, the case is only run on manual invocation.
  triggers?: {
    // Substrings matched against the goal text (case-insensitive). The case
    // applies iff: at least one `goalIncludes` entry is present (OR), AND every
    // `goalIncludesAll` entry is present (AND — each entry may itself be a
    // `|`-joined OR-group), AND no `goalExcludes` entry is present.
    goalIncludes?: string[];
    goalIncludesAll?: string[];
    goalExcludes?: string[];
    modality?: string[];
  };
  graders: GraderConfig[];
  // Optional: a seed input for manual `runCase()` invocations. Live
  // matches build their input from the actual /job context instead.
  seedInput?: RunInput;
  createdAt: number;
};
