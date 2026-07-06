// app/steps/subtaskCompactionStep.ts
//
// When the orchestrator's accumulated subtaskResults gets long, the prompt
// it ships every iteration grows quadratically (each new iter sees every
// prior output). At ~6+ subtasks this starts wasting tokens AND
// degrading quality (recency bias kicks in, older context gets ignored).
//
// This step keeps the most-recent N subtasks verbatim and replaces the
// older ones with a single "compacted_summary" pseudo-subtask written by
// gpt-4.1-mini. The orchestrator sees: 1 condensed history + N raw recents.
//
// Why a plain LLM summarize and not OpenAI's responses.compact: compact()
// is designed for stateful Responses-API conversations (you give it a
// conversation id or message thread). Our subtaskResults are application
// state, not API conversation state. A direct summarize call is the right
// shape here and gives us full control over what to keep.

import { generateText } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";

import { env } from "@/app/lib/env";
import { appendThought } from "@/app/lib/jobStore";
import { recordCost } from "@/app/lib/costTracker";
import type { SubtaskResult } from "@/app/machines/jobMachine";

// Tune via env. Default keeps last 6 verbatim, compacts the rest.
function keepRecentCount(): number {
  const n = Number(env("DEEP_KEEP_RECENT_SUBTASKS") ?? "6");
  return Number.isFinite(n) && n >= 2 ? Math.min(20, n) : 6;
}

export async function compactSubtasksStep(args: {
  jobId: string;
  subtaskResults: SubtaskResult[];
}): Promise<SubtaskResult[]> {
  "use step";

  const keep = keepRecentCount();
  if (args.subtaskResults.length <= keep) return args.subtaskResults;

  const older = args.subtaskResults.slice(0, args.subtaskResults.length - keep);
  const recent = args.subtaskResults.slice(-keep);

  const modelName = env("CHAT_SUMMARY_MODEL") ?? "gpt-4.1-mini";

  await appendThought(args.jobId, {
    kind: "info",
    text: `compacting ${older.length} older subtasks (keeping last ${keep})`,
    data: { older: older.length, keep, model: modelName },
  });

  const olderSerialized = older
    .map((s, i) => {
      const parts = [
        `[${i + 1}] (${s.kind}) ${s.goal}`,
        s.output ? `  output: ${s.output.slice(0, 1500)}` : "",
        s.citations.length
          ? `  citations: ${s.citations.slice(0, 4).join(", ")}`
          : "",
        s.artifacts.length
          ? `  artifacts: ${s.artifacts.slice(0, 6).join(", ")}`
          : "",
      ].filter(Boolean);
      return parts.join("\n");
    })
    .join("\n\n");

  let compactedText: string;
  try {
    const { text, usage } = await generateText({
      model: textAuxModel(modelName),
      temperature: 0.2,
      system: [
        "You compress an older slice of an agent's subtask history into a",
        "single dense paragraph. The orchestrator will read this instead of",
        "the raw outputs to keep its prompt bounded.",
        "",
        "Keep:",
        "  - What was investigated / produced (one sentence per subtask)",
        "  - Concrete findings (numbers, names, decisions)",
        "  - Citations / artifact paths if relevant",
        "Drop:",
        "  - Process narration ('then I searched for...')",
        "  - Repetition across subtasks",
        "  - Speculation; stick to what was found",
      ].join("\n"),
      prompt: `Older subtask outputs (chronological):\n\n${olderSerialized}`,
    });
    compactedText = text;
    await recordCost({
      jobId: args.jobId,
      model: modelName,
      usage: usage as unknown as
        | { inputTokens?: number; outputTokens?: number }
        | null
        | undefined,
    });
  } catch (err: unknown) {
    // Don't fail the whole orchestrator on compaction failure — surface
    // the original older entries.
    await appendThought(args.jobId, {
      kind: "error",
      text: `compaction failed, keeping originals: ${err instanceof Error ? err.message : String(err)}`,
    });
    return args.subtaskResults;
  }

  // Aggregate citations + artifacts so the orchestrator can still see them.
  const allCitations = new Set<string>();
  const allArtifacts = new Set<string>();
  for (const s of older) {
    for (const c of s.citations) allCitations.add(c);
    for (const a of s.artifacts) allArtifacts.add(a);
  }

  const compactedSubtask: SubtaskResult = {
    id: `compact_${Date.now().toString(36)}`,
    iter: older[0]?.iter ?? 0,
    kind: "synthesize",
    goal: `(compacted summary of ${older.length} earlier subtasks)`,
    output: compactedText,
    artifacts: Array.from(allArtifacts),
    citations: Array.from(allCitations),
    ts: Date.now(),
  };

  await appendThought(args.jobId, {
    kind: "info",
    text: `compacted ${older.length} → 1 (${compactedText.length} chars)`,
    data: {
      compactedFrom: older.length,
      compactedChars: compactedText.length,
    },
  });

  return [compactedSubtask, ...recent];
}
