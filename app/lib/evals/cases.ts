// app/lib/evals/cases.ts
//
// Registered eval cases. Each case is a triggered test that runs
// automatically against matching live /jobs. Adding a case here +
// running the postdeploy `seedEvalCases()` (called lazily from the
// recorder) is all you need — no manual ingestion step.
//
// Cases here capture the *failure patterns we've already burned hours
// on*. The point is regression detection: a code change should not
// silently re-introduce a class of bug we've already fixed. Live
// telemetry then tells us whether each fix actually holds.

import { putCase } from "./store";
import type { EvalCase, GraderConfig, RunInput } from "./types";

type SeedCase = {
  id: string;
  suite: string;
  name: string;
  triggers?: EvalCase["triggers"];
  graders: GraderConfig[];
  seedInput?: RunInput;
};

// Seed list. Stable ids (prefixed `ec_seed_*`) so the recorder can
// upsert without duplicating. To add a new case, append here and ship
// — the next /job that matches will run against it.
const SEED_CASES: SeedCase[] = [
  {
    id: "ec_seed_gmail_fetch_week",
    suite: "external-data-fetch",
    name: "Gmail: summarize this week + save to VFS",
    // Scope tightly to the real regression scenario: a job that BOTH reads the
    // mailbox AND saves the result. The composio-gmail + /workspace-VFS graders
    // below only make sense for that case. A bare mention of "email" (e.g. an
    // automation that summarizes an email already inlined in its event payload)
    // must NOT match — the agent rightly summarizes inline with no fetch/save.
    triggers: {
      goalIncludesAll: ["gmail|inbox|my email|my emails", "save|vfs|store|write to"],
      // Exclude automation-fired runs in BOTH prompt generations: the old
      // wording ("this automation was triggered by") and the current one
      // ("You are running an automation that just fired"). When the wording
      // changed, automation runs silently re-matched this chat-job case and
      // flooded the suite with false partials.
      goalExcludes: [
        "this automation was triggered by",
        "you are running an automation",
      ],
    },
    graders: [
      { type: "code", name: "no_max_retries_exceeded" },
      // "execute_tool" substring-matches both COMPOSIO_EXECUTE_TOOL and the
      // newer COMPOSIO_MULTI_EXECUTE_TOOL the agent actually calls now.
      { type: "code", name: "tool_called", args: { name: "execute_tool" } },
      {
        type: "code",
        name: "tool_called_with_args",
        args: { name: "execute_tool", pattern: "gmail" },
      },
      { type: "code", name: "vfs_path_created", args: { prefix: "/workspace" } },
      { type: "code", name: "duration_under_ms", args: { ms: 12 * 60 * 1000 } },
      { type: "code", name: "cost_under", args: { usd: 2.0 } },
    ],
    seedInput: {
      goal: "Using my gmails, summarize everything this week then save the summary to my vfs",
      modality: "research",
    },
  },
  {
    id: "ec_seed_code_simple_edit",
    suite: "code-flow",
    name: "/code: small edit roundtrip",
    triggers: {
      modality: ["code-edit", "code-feature"],
    },
    graders: [
      { type: "code", name: "no_max_retries_exceeded" },
      { type: "code", name: "duration_under_ms", args: { ms: 15 * 60 * 1000 } },
      { type: "code", name: "cost_under", args: { usd: 3.0 } },
    ],
  },
  {
    id: "ec_seed_vfs_write_read",
    suite: "vfs-roundtrip",
    name: "VFS: writes at least one artifact",
    triggers: {
      goalIncludes: ["save to vfs", "save to my vfs", "write to vfs"],
    },
    graders: [
      { type: "code", name: "no_max_retries_exceeded" },
      { type: "code", name: "vfs_path_created", args: { prefix: "/workspace" } },
      { type: "code", name: "artifact_count_at_least", args: { count: 1 } },
    ],
  },
  {
    id: "ec_seed_github_publish",
    suite: "external-data-push",
    name: "GitHub publish from VFS via Composio",
    triggers: {
      goalIncludes: ["push to github", "publish to github", "commit to github"],
    },
    graders: [
      { type: "code", name: "no_max_retries_exceeded" },
      { type: "code", name: "tool_called", args: { name: "publish_vfs_to_github" } },
    ],
  },
];

// Idempotent seed. Called by the recorder on its first invocation per cold
// start. Always upserts (stable ids) so grader/trigger edits in this file
// actually replace the stored copy — write-if-absent left stale graders in
// Redis after the definitions changed.
let seeded = false;
export async function seedEvalCasesIfNeeded(): Promise<void> {
  if (seeded) return;
  seeded = true;
  for (const c of SEED_CASES) {
    await putCase(c);
  }
}

// Returns every seed case whose triggers match the live /job's goal
// + modality. A live run may match zero, one, or many cases — each
// match produces a separate EvalRun so the grader signal stays
// attributable per-case.
export function matchSeedCases(input: RunInput): SeedCase[] {
  const goal = input.goal.toLowerCase();
  const out: SeedCase[] = [];
  for (const c of SEED_CASES) {
    if (!c.triggers) continue;
    const includesHit =
      !c.triggers.goalIncludes ||
      c.triggers.goalIncludes.some((s) => goal.includes(s.toLowerCase()));
    const includesAllHit =
      !c.triggers.goalIncludesAll ||
      c.triggers.goalIncludesAll.every((group) =>
        group
          .toLowerCase()
          .split("|")
          .some((s) => goal.includes(s.trim()))
      );
    const excludesHit =
      !c.triggers.goalExcludes ||
      !c.triggers.goalExcludes.some((s) => goal.includes(s.toLowerCase()));
    const goalHit = includesHit && includesAllHit && excludesHit;
    const modalityHit =
      !c.triggers.modality ||
      (input.modality && c.triggers.modality.includes(input.modality));
    if (goalHit && modalityHit) out.push(c);
  }
  return out;
}

export { SEED_CASES };
