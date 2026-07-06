// app/tools/claudeCodeTool.ts
//
// "ask_claude_code" — agent-callable tool that delegates a coding task to a
// real Claude Code instance running inside our Vercel Sandbox. The agentOS
// LLM (chat: gpt-4.1, /job executor: gpt-5.4) invokes this whenever a task
// is more code-engineering than conversation: implementing features, editing
// repos, running shell pipelines, debugging.
//
// Claude Code runs with --dangerously-skip-permissions so it can act
// autonomously without per-tool approval prompts. The user opted in by
// configuring this tool; that's how the agent stays fluid.
//
// Per-tenant isolation: each Telegram user gets their own CLAUDE_CONFIG_DIR
// + workdir. One user's sessions, history, and files never bleed into
// another's.

import { tool } from "ai";
import { z } from "zod/v4";

import { runClaudeCode } from "@/app/lib/sandboxClaudeCode";
import { recordCost } from "@/app/lib/costTracker";

export type ClaudeCodeToolContext = {
  // Required: tenant id (channel-qualified userId, e.g. "telegram:123").
  // Used for per-tenant session + workdir isolation. Throws at construction
  // if missing — there's no safe default.
  tenantId: string;
  jobId?: string;
};

// Rough cost approximation per claude-code invocation. Real Claude Code
// usage varies wildly (3 sec → 30 min). We attribute as if every call
// consumed ~50k input + 5k output Sonnet tokens (~$0.20) as a placeholder.
// Real telemetry would parse claude's own usage output; v1 keeps it coarse.
const APPROX_INPUT_TOKENS = 50_000;
const APPROX_OUTPUT_TOKENS = 5_000;
const COST_MODEL_NAME = "claude-3-5-sonnet-20241022";

export function makeClaudeCodeTool(ctx: ClaudeCodeToolContext) {
  if (!ctx.tenantId) {
    throw new Error("ask_claude_code requires a tenantId in context");
  }

  return tool({
    description: [
      "Delegate a coding / engineering task to a coding agent running in a",
      "sandboxed Linux environment. The agent can read and write files, run",
      "shell commands, install packages, and operate on whole codebases —",
      "way more capable than just generating a snippet inline.",
      "",
      "GitHub auth & push chain — IMPORTANT:",
      "  - When repo_url is set, the tenant's Composio-managed GitHub token",
      "    is used to authenticate the clone AND any push_to_branch. You",
      "    do NOT need a personal access token (PAT) from the user. If",
      "    GitHub isn't connected for this tenant the tool returns a",
      "    structured error telling you to ask the user to authorize",
      "    GitHub in Composio — relay that message; don't invent a PAT",
      "    step.",
      "  - PUSH TO AN EXISTING REPO: pass `push_to_branch` on the same call",
      "    where you set `repo_url`. The tool clones the repo, runs the",
      "    task, commits all changes, and pushes the branch in one shot.",
      "    Composio auth is used throughout.",
      "  - MATERIALIZE A /code PROJECT TO A *NEW* REPO (the chain):",
      "      1. Call `composio_execute_tool` against the GitHub toolkit's",
      "         GITHUB_CREATE_REPO action. Pass name + visibility. You",
      "         get back the new repo's clone URL (e.g.",
      "         https://github.com/<owner>/<name>).",
      "      2. Call THIS tool with:",
      "           repo_url       = <the new repo URL from step 1>",
      "           push_to_branch = main          (or whatever branch)",
      "           skip_clone     = true          ← critical for a fresh empty repo",
      "           continue_session = true        ← keep the existing workdir + session",
      "           task           = 'Materialize the current workdir' (or a real",
      "                            terminal task; the tool will commit & push",
      "                            everything currently in the workdir either way)",
      "         skip_clone=true tells the tool to NOT try cloning the empty",
      "         remote (which would fail) and NOT wipe the workdir to make",
      "         room for a clone. Instead it `git init`s the workdir, wires",
      "         `origin` to the new repo via Composio's authed URL, then the",
      "         normal commit + push step takes over.",
      "  - If a /code project (`p_<id>`) is currently in `awaiting_followup`",
      "    and the user says 'publish' / 'push it to GitHub' / 'ship it',",
      "    that's the chain above. Don't fall back to the single-call clone",
      "    path; the workdir already has the work — you just need a remote.",
      "",
      "Engine selection (auto):",
      "  - If ANTHROPIC_API_KEY or AI_GATEWAY_API_KEY is set: Claude Code",
      "    (preferred — the most capable agent harness).",
      "  - Otherwise, falls back to OpenCode driving gpt-5.3-codex (with",
      "    gpt-5.4 as second fallback) via OPENAI_API_KEY. Same in-sandbox",
      "    flow, per-tenant workdirs, --continue session resumption.",
      "",
      "Use this when:",
      "  - The user asks to implement a feature, refactor, debug, or run code",
      "  - The task involves multiple files or shell operations",
      "  - You'd otherwise have to fake `<I would write...>` blocks",
      "  - The user explicitly mentions Claude / claude-code",
      "",
      "Do NOT use this for:",
      "  - One-liner code questions you can answer directly",
      "  - Non-code questions",
      "  - Confirming or echoing a /code project decision back to the user",
      "    — if the user is picking an option from a numbered list YOU just",
      "    offered on a p_<id> project, dispatch `/code attach <id> <task>`",
      "    via outbound chat instead of re-running through this tool.",
      "",
      "Each tenant has a persistent workdir; pass continue_session=true on",
      "follow-up calls to resume the most recent Claude Code session in this",
      "tenant's history. That way multi-turn engineering tasks (e.g. \"now",
      "add tests\") just work.",
      "",
      "Claude Code runs with --dangerously-skip-permissions, so it acts",
      "autonomously without per-tool approval. The user accepted this when",
      "configuring the agent.",
    ].join("\n"),
    inputSchema: z.object({
      task: z
        .string()
        .min(1)
        .describe(
          "The full task description for Claude Code. Be specific — Claude won't see the user's conversation context, only this string."
        ),
      continue_session: z
        .boolean()
        .nullable()
        .describe(
          "When true, resumes the most recent Claude Code session in this tenant's workdir. Use for follow-up requests on the same project."
        ),
      model: z
        .enum(["sonnet", "opus", "haiku"])
        .nullable()
        .describe(
          "Claude model to use. sonnet is the balanced default; opus for the hardest work; haiku for quick tweaks."
        ),
      project: z
        .string()
        .nullable()
        .describe(
          "Optional project subdirectory name. Defaults to 'default'. Use distinct projects to keep multiple parallel engineering tasks in their own working directories."
        ),
      repo_url: z
        .string()
        .url()
        .nullable()
        .describe(
          "Optional GitHub repo to clone into the workdir before Claude runs. When set, the tenant's Composio-managed GitHub token is used to authenticate the clone. Use when the task is 'work on my repo'."
        ),
      base_branch: z
        .string()
        .nullable()
        .describe(
          "Optional starting branch to check out after cloning. Default: repo's default branch."
        ),
      push_to_branch: z
        .string()
        .nullable()
        .describe(
          "When set, after Claude finishes successfully, commit changes and push to this branch on the remote. The user can then open a PR. Requires repo_url."
        ),
      commit_message: z
        .string()
        .nullable()
        .describe(
          "Custom commit message when push_to_branch is set. Defaults to a sensible agentOS marker."
        ),
      skip_clone: z
        .boolean()
        .nullable()
        .describe(
          "When true with repo_url + push_to_branch, do NOT clone the remote — initialize the existing workdir as a fresh git repo (`git init` + add `origin`), then commit + push. Use this when publishing an existing /code project's working directory to a brand-new GitHub repo (chain: composio_execute_tool GITHUB_CREATE_REPO → ask_claude_code with skip_clone=true). Without this flag, cloning would either fail against the empty remote OR wipe the project files via the 'non-empty workdir, no .git' branch."
        ),
    }),
    execute: async (args) => {
      const result = await runClaudeCode({
        prompt: args.task,
        tenantId: ctx.tenantId,
        continueSession: args.continue_session ?? false,
        model: args.model ?? undefined,
        project: args.project ?? undefined,
        repoUrl: args.repo_url ?? undefined,
        baseBranch: args.base_branch ?? undefined,
        pushToBranch: args.push_to_branch ?? undefined,
        commitMessage: args.commit_message ?? undefined,
        skipClone: args.skip_clone ?? false,
      });

      if (ctx.jobId && result.ok) {
        await recordCost({
          jobId: ctx.jobId,
          model: COST_MODEL_NAME,
          usage: {
            inputTokens: APPROX_INPUT_TOKENS,
            outputTokens: APPROX_OUTPUT_TOKENS,
          },
        });
      }

      // Keep the returned text bounded so it doesn't blow our context window
      // when the calling LLM ingests it.
      const trimmed = (result.output ?? "").slice(0, 16_000);
      if (!result.ok) {
        return {
          ok: false,
          error: result.error ?? "unknown claude-code error",
          partial_output: trimmed,
          workdir: result.workdir,
        };
      }
      return {
        ok: true,
        output: trimmed,
        workdir: result.workdir,
        session_log_hint: result.sessionLogHint,
        ...(result.repoPush ? { repo_push: result.repoPush } : {}),
      };
    },
  });
}
