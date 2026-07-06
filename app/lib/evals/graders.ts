// app/lib/evals/graders.ts
//
// Two grader families:
//
//   - "code" — deterministic checks against the RunActual record.
//     Cheap, fast, ideal for objective criteria like "did the
//     agent call the gmail tool" or "did the run finish under 10
//     minutes". Inspired by v0's "code-based grading" tier.
//
//   - "llm" — judge prompt evaluated by a cheap model (gpt-5.4-mini)
//     against a rubric. Returns pass/score/notes. Used sparingly
//     because each invocation costs a model call. Inspired by v0's
//     "LLM-based grading" tier.
//
// Graders run in isolation; one grader's failure doesn't block the
// others. The Run's overall status (pass/fail/partial/error) is
// derived from the aggregate.

import { generateObject } from "ai";
import { z } from "zod";

import { buildLlmArgs } from "@/app/lib/modelRouting";

import type {
  CodeGraderName,
  GraderConfig,
  GraderResult,
  RunActual,
  RunInput,
} from "./types";

// --- code graders -------------------------------------------------------

type CodeGrader = (
  actual: RunActual,
  args: Record<string, unknown> | undefined,
  input: RunInput
) => Pick<GraderResult, "pass" | "notes" | "score">;

const codeGraders: Record<CodeGraderName, CodeGrader> = {
  // pass iff ANY tool call has a name containing the given string
  // (case-insensitive). Useful for "did the agent ever call gmail",
  // "did it touch composio_execute_tool", etc. The match is on the
  // serialized "<name>(args)" string so it catches both name and the
  // toolkit arg in one shot.
  tool_called: (actual, args) => {
    const needle = String(args?.name ?? "").toLowerCase();
    if (!needle) return { pass: false, notes: "tool_called: missing 'name' arg" };
    const hit = actual.toolCalls.some((c) =>
      c.toLowerCase().includes(needle)
    );
    return {
      pass: hit,
      notes: hit
        ? undefined
        : `no tool call matched '${needle}' (saw: ${
            actual.toolCalls.slice(0, 6).join(", ") || "none"
          })`,
    };
  },

  // tool_called_with_args: tool name matches AND the args portion of
  // ANY call to that tool matches the provided regex. Lets you assert
  // "called COMPOSIO_EXECUTE_TOOL with a gmail-flavored payload".
  tool_called_with_args: (actual, args) => {
    const name = String(args?.name ?? "").toLowerCase();
    const pattern = String(args?.pattern ?? "");
    if (!name || !pattern) {
      return {
        pass: false,
        notes: "tool_called_with_args: needs 'name' and 'pattern'",
      };
    }
    let re: RegExp;
    try {
      re = new RegExp(pattern, "i");
    } catch (e) {
      return { pass: false, notes: `bad regex: ${String(e)}` };
    }
    const hit = actual.toolCalls.some(
      (c) => c.toLowerCase().includes(name) && re.test(c)
    );
    return {
      pass: hit,
      notes: hit
        ? undefined
        : `no call to '${name}' matched /${pattern}/i`,
    };
  },

  output_includes: (actual, args) => {
    const needle = String(args?.text ?? "");
    if (!needle) return { pass: false, notes: "output_includes: missing 'text'" };
    const hit = (actual.finalText ?? "").includes(needle);
    return { pass: hit, notes: hit ? undefined : `final text missing '${needle}'` };
  },

  output_matches: (actual, args) => {
    const pattern = String(args?.pattern ?? "");
    if (!pattern) return { pass: false, notes: "output_matches: missing 'pattern'" };
    let re: RegExp;
    try {
      re = new RegExp(pattern);
    } catch (e) {
      return { pass: false, notes: `bad regex: ${String(e)}` };
    }
    const hit = re.test(actual.finalText ?? "");
    return { pass: hit, notes: hit ? undefined : `final text didn't match /${pattern}/` };
  },

  cost_under: (actual, args) => {
    const cap = Number(args?.usd ?? NaN);
    if (!Number.isFinite(cap)) return { pass: false, notes: "cost_under: missing/bad 'usd'" };
    const v = actual.costUsd ?? 0;
    return {
      pass: v <= cap,
      notes: v <= cap ? undefined : `cost $${v.toFixed(3)} > cap $${cap.toFixed(3)}`,
    };
  },

  duration_under_ms: (actual, args) => {
    const cap = Number(args?.ms ?? NaN);
    if (!Number.isFinite(cap))
      return { pass: false, notes: "duration_under_ms: missing/bad 'ms'" };
    const v = actual.durationMs ?? 0;
    return {
      pass: v <= cap,
      notes:
        v <= cap
          ? undefined
          : `duration ${Math.round(v / 1000)}s > cap ${Math.round(cap / 1000)}s`,
    };
  },

  vfs_path_created: (actual, args) => {
    const prefix = String(args?.prefix ?? "");
    if (!prefix) return { pass: false, notes: "vfs_path_created: missing 'prefix'" };
    const hit = actual.artifactPaths.some((p) => p.startsWith(prefix));
    return {
      pass: hit,
      notes: hit
        ? undefined
        : `no artifact under '${prefix}' (saw: ${
            actual.artifactPaths.slice(0, 6).join(", ") || "none"
          })`,
    };
  },

  artifact_count_at_least: (actual, args) => {
    const n = Number(args?.count ?? NaN);
    if (!Number.isFinite(n))
      return { pass: false, notes: "artifact_count_at_least: missing 'count'" };
    return {
      pass: actual.artifactPaths.length >= n,
      notes:
        actual.artifactPaths.length >= n
          ? undefined
          : `only ${actual.artifactPaths.length} artifact(s), need ≥ ${n}`,
    };
  },

  // Catches the historical /job failure mode where the WDK step
  // exhausted its retries silently. Fails if the run's error message
  // contains the well-known WDK signature.
  no_max_retries_exceeded: (actual) => {
    const msg = (actual.errorMessage ?? "").toLowerCase();
    const hit = msg.includes("exceeded max retries");
    return {
      pass: !hit,
      notes: hit ? `WDK retry exhaustion: ${actual.errorMessage}` : undefined,
    };
  },
};

// --- LLM judge ----------------------------------------------------------

const judgeSchema = z.object({
  pass: z.boolean(),
  score: z.number().min(0).max(1),
  notes: z.string().max(400),
});

async function llmJudge(args: {
  name: string;
  rubric: string;
  passThreshold?: number;
  actual: RunActual;
  input: RunInput;
}): Promise<GraderResult> {
  const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0.1 });
  const sys = [
    "You are an eval grader. Score the actual output against the rubric.",
    "Respond with structured JSON: { pass, score (0..1), notes (<400 chars) }.",
    `Pass threshold: score >= ${args.passThreshold ?? 0.7}.`,
  ].join("\n");
  const prompt = [
    `Rubric: ${args.rubric}`,
    "",
    `User goal: ${args.input.goal.slice(0, 1000)}`,
    args.input.modality ? `Modality: ${args.input.modality}` : "",
    "",
    "Actual output (truncated):",
    (args.actual.finalText ?? "").slice(0, 4000),
    "",
    args.actual.toolCalls.length
      ? `Tool calls: ${args.actual.toolCalls.slice(0, 12).join(" | ")}`
      : "Tool calls: (none)",
    args.actual.artifactPaths.length
      ? `Artifacts: ${args.actual.artifactPaths.slice(0, 12).join(", ")}`
      : "",
  ]
    .filter(Boolean)
    .join("\n");

  try {
    const r = await generateObject({
      model: (llm as any).model,
      ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
      ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
      schema: judgeSchema,
      system: sys,
      prompt,
    });
    return {
      grader: "llm",
      name: args.name,
      pass: r.object.pass,
      score: r.object.score,
      notes: r.object.notes,
    };
  } catch (e: any) {
    return {
      grader: "llm",
      name: args.name,
      pass: false,
      notes: `judge error: ${String(e?.message ?? e).slice(0, 200)}`,
    };
  }
}

// --- public dispatch ----------------------------------------------------

export async function runGrader(
  config: GraderConfig,
  actual: RunActual,
  input: RunInput
): Promise<GraderResult> {
  if (config.type === "code") {
    const fn = codeGraders[config.name];
    if (!fn) {
      return {
        grader: "code",
        name: config.name,
        pass: false,
        notes: `unknown code grader '${config.name}'`,
      };
    }
    const r = fn(actual, config.args, input);
    return { grader: "code", name: config.name, ...r };
  }
  return llmJudge({
    name: config.name,
    rubric: config.rubric,
    passThreshold: config.passThreshold,
    actual,
    input,
  });
}

export async function runGraders(
  configs: GraderConfig[],
  actual: RunActual,
  input: RunInput
): Promise<GraderResult[]> {
  const out: GraderResult[] = [];
  for (const c of configs) {
    out.push(await runGrader(c, actual, input));
  }
  return out;
}

// Aggregate pass/fail/partial/error from individual grader results.
// Empty list → "error" (a run with no graders is uninterpretable).
export function aggregateStatus(
  grades: GraderResult[]
): "pass" | "fail" | "partial" | "error" {
  if (grades.length === 0) return "error";
  const pass = grades.filter((g) => g.pass).length;
  if (pass === grades.length) return "pass";
  if (pass === 0) return "fail";
  return "partial";
}
