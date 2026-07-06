// app/lib/solutionMemory.ts
//
// Procedural / "skill" memory — the Hermes-style "never forgets how it solved a
// problem" layer. When a job / automation / code project succeeds we record HOW
// it was solved (the task + the outcome + the tools used). Before tackling a new
// task the agent recalls the closest past solutions and seeds its plan with
// them, so a lesson learned in one place is reused everywhere.
//
// Built on the existing tenant-wide ("shared") vector store in agentMemory.ts —
// no new infra. Formation is DETERMINISTIC: we embed the task once (~$0.00002)
// and store the existing result text. ZERO extra LLM tokens, which matters
// because the user is cost-sensitive. Recall is one embedding + an in-process
// cosine rank.

import {
  addMemory,
  searchMemory,
  listMemory,
  deleteMemory as deleteAgentMemory,
  embedText,
  cosine,
  type MemoryRecord,
  type MemoryHit,
} from "@/app/lib/agentMemory";
import { env } from "@/app/lib/env";

export type SolutionSource = "job" | "automation" | "code" | "chat" | "manual";

export type SolutionMeta = {
  source: SolutionSource;
  task: string;
  outcome: string;
  toolsUsed?: string[];
  artifacts?: string[];
  tags?: string[];
  ref?: string; // jobId / automationId / projectId for backtracking
};

// Skip recording if a near-identical task is already remembered. Recurring jobs
// ("daily standup digest") would otherwise pile up one solution per run.
const DEFAULT_DEDUPE_THRESHOLD = 0.93;

function solutionsEnabled(): boolean {
  return (env("SOLUTION_MEMORY") ?? "1") !== "0";
}

function clampText(s: string, n: number): string {
  const t = (s ?? "").replace(/\s+/g, " ").trim();
  return t.length > n ? t.slice(0, n) : t;
}

// The text we embed + store. Front-loads the task (what was asked) so recall
// keys on the problem, then the approach/outcome so the model can reuse it.
function composeSolutionText(meta: SolutionMeta): string {
  const lines = [`TASK: ${clampText(meta.task, 600)}`];
  if (meta.toolsUsed?.length) {
    lines.push(`TOOLS: ${meta.toolsUsed.slice(0, 12).join(", ")}`);
  }
  lines.push(`OUTCOME: ${clampText(meta.outcome, 1400)}`);
  return lines.join("\n");
}

// Record how a task was solved. Best-effort + idempotent-ish (dedupes by task
// similarity). Never throws — formation must not break the job/automation it
// hangs off of.
export async function recordSolution(args: {
  tenantId: string;
  meta: SolutionMeta;
}): Promise<MemoryRecord | null> {
  if (!solutionsEnabled()) return null;
  const { tenantId, meta } = args;
  if (!meta.task?.trim() || !meta.outcome?.trim()) return null;

  try {
    // Dedupe: if we already remember a near-identical task, refresh nothing and
    // skip — keeps the store dense and recall sharp.
    const taskVec = await embedText(clampText(meta.task, 600));
    const existing = await loadSolutions(tenantId, 200);
    const dupThreshold = Number(
      env("SOLUTION_DEDUPE_THRESHOLD") ?? DEFAULT_DEDUPE_THRESHOLD
    );
    for (const rec of existing) {
      if (cosine(taskVec, rec.vec) >= dupThreshold) {
        return null; // already known
      }
    }

    const text = composeSolutionText(meta);
    return await addMemory({
      tenantId,
      scope: { kind: "shared" },
      kind: "solution",
      text,
      source: meta.source,
      meta: {
        task: clampText(meta.task, 600),
        outcome: clampText(meta.outcome, 1400),
        source: meta.source,
        toolsUsed: meta.toolsUsed ?? [],
        artifacts: meta.artifacts ?? [],
        tags: meta.tags ?? [],
        ref: meta.ref ?? null,
      },
    });
  } catch {
    return null;
  }
}

async function loadSolutions(
  tenantId: string,
  limit: number
): Promise<MemoryRecord[]> {
  const recs = await listMemory({
    tenantId,
    scope: { kind: "shared" },
    limit,
  });
  return recs.filter((r) => r.kind === "solution");
}

// Vector-recall the closest past solutions for a new task. Returns [] on any
// failure (recall is never load-bearing).
export async function recallSolutions(args: {
  tenantId: string;
  query: string;
  topK?: number;
  minScore?: number;
}): Promise<MemoryHit[]> {
  if (!solutionsEnabled()) return [];
  if (!args.query?.trim()) return [];
  try {
    const hits = await searchMemory({
      tenantId: args.tenantId,
      scopes: [{ kind: "shared" }],
      query: args.query,
      topK: args.topK ?? 3,
      kinds: ["solution"],
    });
    const min = args.minScore ?? Number(env("SOLUTION_MIN_SCORE") ?? "0.3");
    return hits.filter((h) => h.score >= min);
  } catch {
    return [];
  }
}

// Compact system-prompt block. Kept terse so it adds little token weight.
export function solutionsToPromptBlock(hits: MemoryHit[]): string {
  if (!hits.length) return "";
  const lines = hits.map((h) => {
    const m = (h.record.meta ?? {}) as {
      task?: string;
      outcome?: string;
      source?: string;
    };
    const task = clampText(m.task ?? h.record.text, 160);
    const outcome = clampText(m.outcome ?? "", 280);
    const src = m.source ? ` (${m.source})` : "";
    return `• ${task}${src}\n  → ${outcome}`;
  });
  return [
    "PAST SOLUTIONS — how you solved similar tasks before. Reuse the working",
    "approach/tools instead of rediscovering them; adapt as needed:",
    lines.join("\n"),
  ].join("\n");
}

// --- UI surface -------------------------------------------------------------

export type SolutionView = {
  id: string;
  task: string;
  outcome: string;
  source: string;
  tags: string[];
  ref: string | null;
  ts: number;
};

export async function listSolutions(args: {
  tenantId: string;
  limit?: number;
}): Promise<SolutionView[]> {
  const recs = await loadSolutions(args.tenantId, args.limit ?? 50);
  return recs.map((r) => {
    const m = (r.meta ?? {}) as Record<string, unknown>;
    return {
      id: r.id,
      task: String(m.task ?? r.text),
      outcome: String(m.outcome ?? ""),
      source: String(m.source ?? r.source ?? "manual"),
      tags: Array.isArray(m.tags) ? (m.tags as string[]) : [],
      ref: m.ref ? String(m.ref) : null,
      ts: r.ts,
    };
  });
}

export async function deleteSolution(id: string): Promise<void> {
  await deleteAgentMemory(id);
}
