// app/tools/askGpt5Tool.ts
//
// "ask_gpt5" — exposed to agentTurn so the chat agent (which runs on gpt-4.1
// by default) can escalate hard questions to gpt-5 without us hand-wiring
// every escalation point. Stays cheap by:
//
//   - Defaulting to gpt-5.4-mini for simple / medium complexity
//   - Escalating only to gpt-5.4 (NEVER o3 / o3-pro) for complex questions
//   - Sending only the question text, no big context dumps
//
// Cost ceiling per call: ~$0.15 worst case.

import { tool, generateText } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs, type Purpose } from "@/app/lib/modelRouting";
import { recordCost } from "@/app/lib/costTracker";

export type AskGpt5ToolContext = {
  // When set, cost from ask_gpt5 calls is rolled up into the job's running
  // total. When unset (plain chat), the call still happens but isn't tracked
  // per-job.
  jobId?: string;
};

export function makeAskGpt5Tool(ctx: AskGpt5ToolContext) {
  return tool({
    description: [
      "Consult gpt-5 (a more capable model) when you need help answering a",
      "hard question or one that depends on knowledge you might not have.",
      "Use this when:",
      "  - The user asks something you're uncertain about",
      "  - A question requires multi-step reasoning beyond your comfort zone",
      "  - You'd otherwise have to hedge or refuse",
      "",
      "Pick complexity_hint carefully — the model + thinking budget scales:",
      "  simple   — single-fact lookup, definition; uses gpt-5.4-mini, no extra thinking",
      "  medium   — short reasoning chain; uses gpt-5.4-mini with low effort",
      "  complex  — multi-step reasoning, technical depth; uses gpt-5.4 with high effort",
      "",
      "Pass enough context in `question` that the consulted model can answer",
      "without follow-ups — it can't see the current conversation.",
    ].join("\n"),
    inputSchema: z.object({
      question: z
        .string()
        .min(1)
        .describe(
          "The question you want gpt-5 to answer. Include any context it needs."
        ),
      complexity_hint: z
        .enum(["simple", "medium", "complex"])
        .nullable()
        .describe(
          "How hard the question is. Drives model + reasoning-effort choice."
        ),
    }),
    execute: async (args) => {
      const hint = args.complexity_hint ?? "medium";

      // Map complexity → purpose tier. We deliberately cap at `smart`
      // (gpt-5.4) to keep cost bounded; complex problems can still get a
      // reasoning model, but never the premium o3-pro tier.
      let purpose: Purpose;
      let effortOverride: "low" | "medium" | "high" | null;
      if (hint === "complex") {
        purpose = "smart";
        effortOverride = "high";
      } else if (hint === "medium") {
        purpose = "fast-meta";
        effortOverride = "low";
      } else {
        purpose = "fast-meta";
        effortOverride = null;
      }

      const llm = buildLlmArgs({ purpose, temperature: 0.3 });

      // If the resolved model is a reasoning model, our builder already
      // attached providerOptions.openai.reasoningEffort = high. Override
      // when the caller wants a softer/cheaper run.
      const providerOptions = effortOverride
        ? { openai: { reasoningEffort: effortOverride } }
        : llm.providerOptions;

      const system = [
        "You are a more capable model being consulted by another agent that hit",
        "a question it isn't confident about. Answer directly and concisely. If",
        "you also don't know, say so — don't fabricate. If the question is",
        "ambiguous and you must guess, state what you assumed.",
      ].join("\n");

      try {
        const result = await generateText({
          model: (llm as any).model,
          ...(providerOptions ? { providerOptions } : {}),
          ...(typeof llm.temperature === "number" && purpose !== "smart"
            ? { temperature: llm.temperature }
            : {}),
          system,
          prompt: args.question,
        });

        if (ctx.jobId) {
          await recordCost({
            jobId: ctx.jobId,
            model: llm.modelName,
            usage: (result as any).usage,
          });
        }

        return {
          answer: result.text,
          modelUsed: llm.modelName,
          complexity: hint,
        };
      } catch (err: any) {
        return {
          answer: "",
          error: err?.message ?? String(err),
          modelUsed: llm.modelName,
          complexity: hint,
        };
      }
    },
  });
}
