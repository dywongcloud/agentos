// app/tools/browseWebTool.ts
//
// AI SDK tool that exposes the headless browser to agentTurn. The agent
// invokes this autonomously when it needs to read the current web — the user
// never has to type a slash command.
//
// Implementation lives in app/lib/sandboxBrowser.ts. This file is just the
// tool surface + post-call cost accounting.

import { tool } from "ai";
import { z } from "zod/v4";

import { browseWeb } from "@/app/lib/sandboxBrowser";
import { recordCost } from "@/app/lib/costTracker";

export type BrowseWebToolContext = {
  // When set, the rough cost of the browse session is rolled up into the
  // job's running spend. computer-use-preview pricing is approximated by
  // attributing per-action cost as if it were a smart-model call.
  jobId?: string;
  // Tenant id (channel-qualified userId, e.g. "telegram:123"). Required for
  // browser auth state load/save — without it, every browse runs in a
  // clean profile.
  tenantId?: string;
};

// Coarse per-action cost approximation for budget tracking. The
// computer-use-preview pricing isn't public in a stable form; this is a
// conservative estimate aimed at NOT under-counting.
const APPROX_INPUT_TOKENS_PER_ACTION = 1500;
const APPROX_OUTPUT_TOKENS_PER_ACTION = 200;
const COMPUTER_USE_MODEL_FOR_BILLING = "gpt-5.4";

export function makeBrowseWebTool(ctx: BrowseWebToolContext) {
  return tool({
    description: [
      "Browse the live web using a real headless Chrome browser. Use this when",
      "you need information that depends on the current state of the internet",
      "(recent news, prices, status pages, public data not in training),",
      "or when you must interact with a page (click buttons, navigate menus,",
      "search a specific site). Do NOT use for math, code, or general knowledge",
      "that doesn't depend on the live web.",
      "",
      "How it works:",
      "  - A persistent Vercel Sandbox runs Chromium + Playwright.",
      "  - A Gemini 3.1 Pro side-car first turns your goal into a multi-step",
      "    navigation plan (best start URL, what to click, pitfalls to expect),",
      "    so multi-page tasks navigate far more reliably.",
      "  - The browser is then driven by OpenAI's computer-use-preview model,",
      "    which takes screenshots, acts (click, type, scroll), and extracts",
      "    the relevant information for the goal.",
      "  - Hard cap: 15 actions or ~3 minutes wall-clock, whichever comes first.",
      "",
      "Best results:",
      "  - State the goal clearly in 1-2 sentences",
      "  - Provide a start_url when you know the right starting point (a specific",
      "    site is usually faster than starting at google.com)",
      "  - One goal per call — don't chain unrelated objectives",
    ].join("\n"),
    inputSchema: z.object({
      goal: z
        .string()
        .min(1)
        .describe(
          "What you want to find / do on the web. Be specific. The browser model only sees this and the page screenshots."
        ),
      start_url: z
        .string()
        .url()
        .nullable()
        .describe(
          "Optional URL to open first. Leave null to start at a search engine."
        ),
      max_actions: z
        .number()
        .int()
        .min(1)
        .max(30)
        .nullable()
        .describe(
          "Hard cap on browser actions for this call (default 15). Increase for multi-page workflows; keep low for quick lookups."
        ),
    }),
    execute: async (args) => {
      const result = await browseWeb({
        goal: args.goal,
        startUrl: args.start_url ?? undefined,
        maxActions: args.max_actions ?? 15,
        tenantId: ctx.tenantId,
      });

      if (ctx.jobId && result.ok) {
        // Approximate cost based on action count. Better than nothing for
        // budget enforcement; refine when computer-use-preview pricing
        // stabilizes or we capture real usage from the Responses API.
        const actions = result.actionsTaken ?? 0;
        await recordCost({
          jobId: ctx.jobId,
          model: COMPUTER_USE_MODEL_FOR_BILLING,
          usage: {
            inputTokens: actions * APPROX_INPUT_TOKENS_PER_ACTION,
            outputTokens: actions * APPROX_OUTPUT_TOKENS_PER_ACTION,
          },
        });
      }

      // Return a shape the calling LLM can reason over. Trim the result text
      // so we don't blow the context window if the page was huge.
      const trimmed = (result.result ?? "").slice(0, 12_000);
      if (!result.ok) {
        return {
          ok: false,
          error: result.error ?? "unknown browse error",
          partial_result: trimmed,
          final_url: result.finalUrl,
        };
      }
      return {
        ok: true,
        result: trimmed,
        final_url: result.finalUrl,
        actions_taken: result.actionsTaken,
        hit_cap: result.hitCap ?? false,
      };
    },
  });
}
