// app/lib/browserBrain.ts
//
// Gemini 3.1 Pro "side-car" for browser operations. The headless browser is
// driven by OpenAI's computer-use-preview model inside a sandbox — it only
// ever sees the goal string + page screenshots. That model is great at the
// mechanical screenshot→action loop but weak at PLANNING a multi-step web
// task ("find X, which means I should first search Y, then filter by Z, then
// open the third result and read the table").
//
// So before we hand the goal to the screenshot loop, we run it through a
// reasoning model (Gemini 3.1 Pro by default, via the `browser-pro` purpose)
// that:
//   - restates the goal crisply
//   - proposes the best starting URL
//   - lays out the concrete navigation steps it expects
//   - calls out likely obstacles (cookie walls, logins, pagination, search
//     box selectors) and how to handle them
//   - says exactly what to extract and in what shape
//
// The enriched plan is concatenated with the original goal and passed to the
// computer-use loop as its goal. Net effect: the browser "thinks for itself"
// far better on multi-page tasks.
//
// Everything here is best-effort and non-breaking: if Gemini isn't
// configured or the call fails, we return the original goal unchanged and the
// browse proceeds exactly as before.

import { generateObject } from "ai";
import { z } from "zod/v4";

import { env } from "@/app/lib/env";
import { resolveModelName, resolveModel, providerFor } from "@/app/lib/modelRouting";

export type EnrichedPlan = {
  enrichedGoal: string;
  suggestedStartUrl?: string;
  steps: string[];
  // Which model produced this — for logging / debugging.
  model: string;
  // false when enrichment was skipped (no key) or failed — caller then uses
  // the raw goal.
  enriched: boolean;
};

const planSchema = z.object({
  restated_goal: z
    .string()
    .describe("The goal restated crisply and unambiguously for the browser agent."),
  suggested_start_url: z
    .string()
    .nullable()
    .describe("The single best URL to open first, or null to start at a search engine."),
  steps: z
    .array(z.string())
    .max(12)
    .describe("Ordered, concrete navigation steps the browser should take."),
  watch_out_for: z
    .array(z.string())
    .max(8)
    .describe("Likely obstacles (cookie banners, logins, pagination) + how to get past them."),
  extract: z
    .string()
    .describe("Exactly what information to pull out and in what shape."),
});

// Is the browser side-car available? True when Gemini (or whatever
// browser-pro resolves to) has a usable key. We check the Google key when the
// resolved model is Gemini, else the OpenAI key (browser-pro falls back to a
// smart OpenAI model).
function sidecarAvailable(): boolean {
  const modelName = resolveModelName("browser-pro");
  if (providerFor(modelName) === "google") {
    return !!env("GOOGLE_GENERATIVE_AI_API_KEY") || !!env("GOOGLE_API_KEY");
  }
  return !!env("OPENAI_API_KEY");
}

export async function enrichBrowseGoal(args: {
  goal: string;
  startUrl?: string;
}): Promise<EnrichedPlan> {
  const modelName = resolveModelName("browser-pro");

  // Opt-out + availability guards. Either returns the raw goal untouched.
  if ((env("BROWSER_SIDECAR_ENABLED") ?? "true") === "false" || !sidecarAvailable()) {
    return {
      enrichedGoal: args.goal,
      suggestedStartUrl: args.startUrl,
      steps: [],
      model: modelName,
      enriched: false,
    };
  }

  const system = [
    "You are the planning brain for a web-browsing agent. A separate, simpler",
    "model will actually drive the browser (it takes screenshots and clicks).",
    "Your job is to think hard about the user's goal and produce a tight plan",
    "that the driver can follow step by step.",
    "",
    "Be concrete and web-aware: name the kind of page to look for, the search",
    "terms to type, which result to click, how to handle pagination, cookie",
    "walls, and login prompts. Don't be vague. Assume the driver is literal —",
    "it does exactly what each step says and nothing more.",
  ].join("\n");

  const prompt = [
    `Goal: ${args.goal}`,
    args.startUrl ? `Suggested start URL from caller: ${args.startUrl}` : "",
    "",
    "Produce the plan.",
  ]
    .filter(Boolean)
    .join("\n");

  try {
    const { object } = await generateObject({
      model: resolveModel(modelName),
      schema: planSchema,
      system,
      prompt,
      temperature: 0.3,
    });

    const enrichedGoal = [
      `PRIMARY GOAL: ${object.restated_goal}`,
      "",
      "NAVIGATION PLAN (follow in order, adapt if the page differs):",
      ...object.steps.map((s, i) => `  ${i + 1}. ${s}`),
      "",
      object.watch_out_for.length
        ? "WATCH OUT FOR:\n" + object.watch_out_for.map((w) => `  - ${w}`).join("\n")
        : "",
      "",
      `WHAT TO EXTRACT: ${object.extract}`,
      "",
      `(Original user goal, verbatim: ${args.goal})`,
    ]
      .filter(Boolean)
      .join("\n");

    return {
      enrichedGoal,
      suggestedStartUrl: object.suggested_start_url ?? args.startUrl,
      steps: object.steps,
      model: modelName,
      enriched: true,
    };
  } catch (err: any) {
    console.warn(
      `[browserBrain] enrichment failed (${modelName}), using raw goal: ${err?.message ?? String(err)}`
    );
    return {
      enrichedGoal: args.goal,
      suggestedStartUrl: args.startUrl,
      steps: [],
      model: modelName,
      enriched: false,
    };
  }
}
