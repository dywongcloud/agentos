// app/steps/webSearchStep.ts
//
// Native web search subagent built on OpenAI's Responses API + the
// `web_search_preview` tool. This is the closest available primitive to
// ChatGPT Deep Research's underlying search step: the model decides what to
// search for, follows up, and returns findings with citations baked in.
//
// Wiring:
//   - Triggered by jobWorkflow when ctx.modality === "research" (or when env
//     OPENAI_WEB_SEARCH_FORCE=true).
//   - Returns a findings text block + citations. The executor step then
//     receives these via the prompt so agentTurn can synthesize the final
//     answer using them.
//
// Env:
//   OPENAI_WEB_SEARCH_ENABLED      "true" to enable the subagent
//   OPENAI_WEB_SEARCH_MODEL        Responses-capable model name; defaults
//                                  to SMART_MODEL_NAME or "gpt-4.1"
//   OPENAI_WEB_SEARCH_CONTEXT_SIZE "low" | "medium" | "high"   default "high"

import { generateText } from "ai";
import { openai } from "@ai-sdk/openai";

import { env } from "@/app/lib/env";
import { appendThought } from "@/app/lib/jobStore";
import { recordCost } from "@/app/lib/costTracker";

export type WebSearchFindings = {
  findings: string;
  citations: string[];
  modelName: string;
};

export function isWebSearchEnabled(): boolean {
  return env("OPENAI_WEB_SEARCH_ENABLED") === "true";
}

export function webSearchModelName(): string {
  return env("OPENAI_WEB_SEARCH_MODEL") ?? env("SMART_MODEL_NAME") ?? "gpt-4.1";
}

function contextSize(): "low" | "medium" | "high" {
  const raw = (env("OPENAI_WEB_SEARCH_CONTEXT_SIZE") ?? "high").toLowerCase();
  if (raw === "low" || raw === "medium" || raw === "high") return raw;
  return "high";
}

export async function webSearchStep(args: {
  jobId: string;
  query: string;
}): Promise<WebSearchFindings> {
  "use step";

  const modelName = webSearchModelName();

  await appendThought(args.jobId, {
    kind: "tool",
    text: `web search: start (model=${modelName})`,
    data: { query: args.query, contextSize: contextSize() },
  });

  const tools = {
    web_search_preview: openai.tools.webSearchPreview({
      searchContextSize: contextSize(),
    } as any),
  };

  try {
    const result = await generateText({
      model: openai.responses(modelName),
      tools,
      prompt: [
        "You are a research subagent. Search the open web to answer the user's",
        "query thoroughly. Quote concrete facts and cite sources by URL. Cover",
        "multiple angles when relevant. Do not produce the final user-facing",
        "answer — produce a researcher's notes that another agent will use to",
        "write the final answer.",
        "",
        `Query: ${args.query}`,
      ].join("\n"),
    });

    const findings = String((result as any).text ?? "").trim();

    await recordCost({
      jobId: args.jobId,
      model: modelName,
      usage: (result as any).usage,
      promptText: args.query,
      outputText: findings,
    });

    // Extract URLs from sources (AI SDK v5 surfaces them as result.sources
    // when supported by the provider) and fall back to scanning the text.
    const sourceUrls = new Set<string>();
    const sources = (result as any).sources;
    if (Array.isArray(sources)) {
      for (const s of sources) {
        const url = s?.url ?? s?.id ?? "";
        if (typeof url === "string" && /^https?:\/\//.test(url)) {
          sourceUrls.add(url);
        }
      }
    }
    if (sourceUrls.size === 0) {
      const re = /https?:\/\/[^\s)\]]+/g;
      for (const m of findings.matchAll(re)) sourceUrls.add(m[0]);
    }

    const citations = Array.from(sourceUrls).slice(0, 32);

    await appendThought(args.jobId, {
      kind: "result",
      text: `web search: complete — ${citations.length} source(s), ${findings.length} chars`,
      data: { citations },
    });

    return { findings, citations, modelName };
  } catch (err: any) {
    await appendThought(args.jobId, {
      kind: "error",
      text: `web search failed: ${err?.message ?? String(err)}`,
    });
    return { findings: "", citations: [], modelName };
  }
}
