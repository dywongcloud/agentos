// app/lib/jobAsk.ts
//
// Read-only side-channel: answer "what is job X doing / thinking?" without
// touching the actor or its workflow. Reads job meta + most recent thoughts
// and runs a fast model over them. Safe to call concurrently with a running
// job because nothing here writes to the job's snapshot or status.

import { generateText } from "ai";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import {
  getJobMeta,
  getThoughts,
  type JobMeta,
  type Thought,
} from "@/app/lib/jobStore";
import { recordCost } from "@/app/lib/costTracker";

export type AskResult = {
  ok: true;
  jobId: string;
  status: JobMeta["status"];
  answer: string;
  contextThoughts: number;
};

export type AskMissing = { ok: false; reason: "not_found" };

const DEFAULT_THOUGHTS = 40;

export async function askJob(args: {
  jobId: string;
  question: string;
  thoughtLimit?: number;
}): Promise<AskResult | AskMissing> {
  const meta = await getJobMeta(args.jobId);
  if (!meta) return { ok: false, reason: "not_found" };

  const limit = Math.max(1, Math.min(200, args.thoughtLimit ?? DEFAULT_THOUGHTS));
  const thoughts = await getThoughts(args.jobId, { limit });

  const llm = buildLlmArgs({ purpose: "fast-meta", temperature: 0.3 });

  const system = [
    "You answer questions about a running agent job, using only the provided",
    "metadata and thought log. The thought log is newest-first. Be concise",
    "and concrete. If the thought log doesn't have the answer, say so plainly —",
    "do NOT speculate about steps that haven't happened yet.",
  ].join("\n");

  const userPrompt = [
    `Job: ${meta.jobId}`,
    `Status: ${meta.status}`,
    `Kind: ${meta.kind}`,
    `Started: ${new Date(meta.createdAt).toISOString()}`,
    meta.startedAt ? `Began executing: ${new Date(meta.startedAt).toISOString()}` : "",
    `Original prompt:`,
    meta.prompt,
    "",
    `Most recent ${thoughts.length} thought entries (newest first):`,
    formatThoughts(thoughts),
    "",
    `User question: ${args.question}`,
  ]
    .filter(Boolean)
    .join("\n");

  const result = await generateText({
    model: (llm as any).model,
    ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
    ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
    system,
    prompt: userPrompt,
  });
  const text = result.text;
  await recordCost({
    jobId: args.jobId,
    model: llm.modelName,
    usage: (result as any).usage,
  });

  return {
    ok: true,
    jobId: meta.jobId,
    status: meta.status,
    answer: text.trim(),
    contextThoughts: thoughts.length,
  };
}

function formatThoughts(thoughts: Thought[]): string {
  return thoughts
    .map((t) => {
      const tsIso = new Date(t.ts).toISOString().slice(11, 19);
      const data =
        t.data && Object.keys(t.data).length
          ? ` | ${JSON.stringify(t.data).slice(0, 240)}`
          : "";
      return `[${tsIso}] (${t.kind}) ${t.text}${data}`;
    })
    .join("\n");
}
