// app/lib/sandboxClaudeCode.ts
//
// Run Claude Code (the official Anthropic CLI) inside our shared Vercel
// Sandbox. Reuses the same `claw-browser` sandbox the browser tool uses —
// claude-code is just another binary installed alongside agent-browser, so
// the install cost is shared and warm sandboxes serve both.
//
// Design choices, with reasoning:
//
//   1. `claude --print --dangerously-skip-permissions` — non-interactive
//      mode + auto-approve every tool call. The agentOS is the user-facing
//      surface; Claude Code's per-call permission prompts would deadlock
//      our flow. The flag is documented and intentional.
//
//   2. Per-tenant CLAUDE_CONFIG_DIR — every tenant's session history,
//      auth state, and project-local config live in their own directory at
//      `/tmp/claw-browser/cc-workdirs/{tenantId}/.claude/`. Stops one
//      Telegram user's sessions from leaking into another's.
//
//   3. `--continue` for follow-ups — Claude Code maintains a session log
//      per workdir. When the agent calls us with `continueSession=true`,
//      we add `--continue` so claude picks up where it left off. Lets a
//      single deep engineering task span multiple Telegram messages.
//
//   4. OpenRouter detection — keys starting with `sk-or-` get routed via
//      ANTHROPIC_BASE_URL=https://openrouter.ai/api. Matches the pattern
//      from the agentbox-desktop-demo reference. If the user has only an
//      OpenRouter key, it Just Works.
//
//   5. Long-task durability via sandbox `runCommand` — Vercel Sandbox
//      commands can run up to the sandbox's own timeout (we set 1h idle).
//      For tasks that need to span function invocations we use detached
//      mode; for v1 most tasks complete inside the call.
//
// What's NOT here (next slice if needed):
//   - Streaming Claude's intermediate output to Telegram (currently we wait
//     for completion and return final stdout).
//   - File-artifact extraction (Claude's edits live in the workdir; v2 can
//     surface them as VFS files).

import { Sandbox } from "@vercel/sandbox";
import { env } from "@/app/lib/env";
import { getGithubTokenForTenant } from "@/app/lib/composioGithub";

const SANDBOX_NAME = "claw-browser";
const SANDBOX_DIR = "/tmp/claw-browser";
const CC_WORKDIRS = `${SANDBOX_DIR}/cc-workdirs`;

export type ClaudeCodeRequest = {
  // Required: the task / prompt to give Claude Code.
  prompt: string;
  // Required: tenant id for per-user session isolation.
  tenantId: string;
  // When true, claude --continue picks up the most recent session in this
  // tenant's workdir. When false (default), starts fresh.
  continueSession?: boolean;
  // Optional: model alias. Defaults from CLAUDE_CODE_MODEL env or "sonnet".
  model?: string;
  // Optional: per-task subdirectory within the tenant workdir. Useful for
  // isolating "different projects" the agent is working on. Defaults to
  // "default".
  project?: string;
  // Optional: absolute path inside the sandbox to use as the workdir,
  // overriding the per-tenant `${CC_WORKDIRS}/${tenant}/${project}` layout.
  // Used by the codeWorkflow to give each long-running /code project a
  // stable, project-scoped workdir that survives across turns (so
  // `claude --continue` resumes the right session log).
  absoluteWorkdir?: string;
  // Optional: engine to use, overriding chooseEngine(). Used by codeWorkflow
  // when a project was created under a specific engine and must stay on it.
  forceEngine?: "claude" | "opencode";
  // Hard cap on wall-clock for this call. Default 8 minutes. The Vercel
  // function timeout itself caps higher.
  timeoutMs?: number;
  // Optional GitHub repo flow. When set, we:
  //   1. Pull a GitHub OAuth token via Composio for this tenant
  //   2. Clone <repoUrl> into the workdir with token-authed URL
  //   3. Run Claude inside the cloned repo
  //   4. (if pushToBranch is set) commit + push to that branch
  // If the tenant has no Composio GitHub connection, the call fails with a
  // friendly setup message — caller surfaces it.
  repoUrl?: string;
  // Optional starting branch to check out before Claude runs. Default: the
  // repo's default branch.
  baseBranch?: string;
  // Optional. When set, after Claude finishes successfully we create this
  // branch, commit any changes with `commitMessage` (default sensible), and
  // push to origin. Useful for the "make a PR-ready change" flow.
  pushToBranch?: string;
  // Optional commit message. Defaults to a stamped agentOS message.
  commitMessage?: string;
  // When true and `repoUrl` is set, do NOT clone the remote into the
  // workdir. Instead, treat the existing workdir contents as the source
  // of truth, `git init` if needed, add the remote as `origin`, and
  // proceed to engine + commit + push. This is the path the agent uses
  // to publish a /code project's working directory to a freshly-created
  // (and therefore empty) GitHub repo: no clone is possible against an
  // empty remote, and cloning would also wipe the project's files via
  // the "non-empty workdir, no .git" branch.
  skipClone?: boolean;
  // Optional progress callback. Called once per major phase
  // ("preparing sandbox", "cloning repo", "engine running"). Used by
  // runCodeTurnStep to append per-phase entries to the project log so
  // `/code status` shows what's happening during the long blackout between
  // sandbox bootstrap and engine completion.
  onProgress?: (phase: string) => Promise<void> | void;
  // When true, the engine command is started in the sandbox in *detached*
  // mode and runClaudeCode returns immediately with the cmdId stashed in
  // `error` (prefixed `__cmdId:`). The actual completion is awaited later
  // by awaitClaudeCommand on a future function invocation, so a long-
  // running engine survives Vercel function timeouts.
  detached?: boolean;
};

export type ClaudeCodeResult = {
  ok: boolean;
  output: string;
  exitCode: number;
  workdir: string;
  // Path inside the sandbox to the session log Claude wrote, if findable.
  // Useful for /code continue UX.
  sessionLogHint?: string;
  // Set when the repo-flow ran. Describes whether the push succeeded and the
  // branch+commit refs the agent can show the user.
  repoPush?: {
    attempted: boolean;
    ok: boolean;
    branch?: string;
    commitMessage?: string;
    error?: string;
  };
  error?: string;
};

// Result of dispatching the engine in detached mode. The caller then has
// to await completion separately (via awaitDetachedClaudeCommand) so the
// long-running engine survives Vercel function-instance teardown.
export type ClaudeCodeDispatch =
  | {
      ok: true;
      cmdId: string;
      workdir: string;
    }
  | {
      ok: false;
      error: string;
      workdir: string;
    };

export type ClaudeCodeFinishResult =
  | { done: false }
  | {
      done: true;
      ok: boolean;
      output: string;
      exitCode: number;
      sessionLogHint?: string;
      error?: string;
    };

const DEFAULT_TIMEOUT_MS = 8 * 60 * 1000;

// Wall-clock guard around a Sandbox command's stdout/stderr awaits. The
// Vercel Sandbox client doesn't currently surface a per-command timeout, and
// `cmd.stdout()` / `cmd.stderr()` block indefinitely when the underlying
// process hangs (claude waiting on an auth-resolve loop, opencode looping on
// model fallback, sandbox networking blip mid-stream). Without this guard a
// single stuck turn freezes the codeWorkflow forever — no VFS output, no
// failure to retry against.
async function withDeadline<T>(
  p: Promise<T>,
  ms: number,
  label: string
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`timeout after ${Math.round(ms / 1000)}s: ${label}`)),
      ms
    );
  });
  try {
    return await Promise.race([p, deadline]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function sanitizeTenantId(t: string): string {
  // tenantIds look like "telegram:123456789" — keep the channel + id but
  // strip any path separators or other shell-unfriendly chars.
  return t.replace(/[^a-zA-Z0-9._-]/g, "_");
}

function workdirFor(tenantId: string, project: string): string {
  return `${CC_WORKDIRS}/${sanitizeTenantId(tenantId)}/${project || "default"}`;
}

function configDirFor(tenantId: string): string {
  return `${CC_WORKDIRS}/${sanitizeTenantId(tenantId)}/.claude`;
}

// Build the env vars Claude Code will see inside the sandbox.
//
// Auth precedence (matches what feels natural across the available options):
//   1. ANTHROPIC_API_KEY (sk-ant-…)          — direct Anthropic
//   2. ANTHROPIC_API_KEY (sk-or-…)           — OpenRouter Anthropic-compat
//   3. AI_GATEWAY_API_KEY                    — Vercel AI Gateway
//   4. CLAUDE_ANTHROPIC_BASE_URL override    — self-hosted proxy
//
// (1) is the reference; (2) is the OpenRouter convenience pattern from
// agentbox-desktop-demo; (3) is the vercel-labs/coding-agent-template
// default. Adding (3) as a fallback means anyone with a working Vercel AI
// Gateway setup can use Claude Code without provisioning a separate
// Anthropic key.
function buildClaudeEnv(tenantId: string): Record<string, string> {
  const out: Record<string, string> = {};
  const anthropicKey = env("ANTHROPIC_API_KEY");
  const aiGatewayKey = env("AI_GATEWAY_API_KEY");

  if (anthropicKey) {
    out.ANTHROPIC_API_KEY = anthropicKey;
    if (anthropicKey.startsWith("sk-or-")) {
      out.ANTHROPIC_BASE_URL =
        env("CLAUDE_OPENROUTER_BASE_URL") ?? "https://openrouter.ai/api";
    } else if (env("CLAUDE_ANTHROPIC_BASE_URL")) {
      out.ANTHROPIC_BASE_URL = env("CLAUDE_ANTHROPIC_BASE_URL")!;
    }
  } else if (aiGatewayKey) {
    // Vercel AI Gateway: same env name claude-code expects, different value
    // + base URL. Reference: vercel-labs/coding-agent-template uses this as
    // the default integration with Vercel projects.
    out.ANTHROPIC_API_KEY = aiGatewayKey;
    out.ANTHROPIC_BASE_URL =
      env("AI_GATEWAY_BASE_URL") ?? "https://ai-gateway.vercel.sh";
  }

  // Per-tenant config dir so session histories don't bleed across users.
  out.CLAUDE_CONFIG_DIR = configDirFor(tenantId);

  // Be explicit about non-interactive; some terminal features check TERM.
  out.TERM = "xterm-256color";

  return out;
}

// Whether any of the Claude-side auth paths are configured.
function hasClaudeAuth(): boolean {
  return !!env("ANTHROPIC_API_KEY") || !!env("AI_GATEWAY_API_KEY");
}

// Choose which CLI we'll drive inside the sandbox. Claude wins when its auth
// is set; otherwise we try opencode (sst's open-source CLI) with the user's
// OPENAI_API_KEY. Returns "none" when neither is configured.
type Engine = "claude" | "opencode" | "none";

function chooseEngine(): Engine {
  if (hasClaudeAuth()) return "claude";
  if (env("OPENAI_API_KEY")) return "opencode";
  return "none";
}

function opencodeModel(): string {
  // OpenCode uses provider-qualified model ids: "openai/<model>".
  // Default chain: gpt-5.3-codex (purpose-built for code), then gpt-5.4 (general).
  const direct = env("OPENCODE_MODEL_NAME");
  if (direct) return direct;
  return "openai/gpt-5.3-codex";
}
function opencodeFallbackModel(): string {
  return env("OPENCODE_FALLBACK_MODEL") ?? "openai/gpt-5.4";
}

let sandboxPromise: Promise<Sandbox> | null = null;

// Sandbox bootstrap is identical to the browser tool's — we share the same
// named persistent sandbox, so the install cost is paid once for both
// agent-browser and claude-code. This import is kept lazy to avoid a cycle.
async function getSharedSandbox(): Promise<Sandbox> {
  if (sandboxPromise) {
    try {
      return await sandboxPromise;
    } catch {
      sandboxPromise = null;
    }
  }
  sandboxPromise = (async () => {
    // Delegate to the existing browser-tool bootstrap so we don't duplicate
    // dnf/install logic. That function is internal to sandboxBrowser.ts;
    // import it via dynamic import to dodge any circular import surprises.
    const mod = await import("@/app/lib/sandboxBrowser");
    // sandboxBrowser doesn't currently export its sandbox getter directly.
    // Easiest path: call browseWeb-equivalent of "ensure sandbox" — but we
    // don't have that. So we replicate the Sandbox.getOrCreate call here.
    void mod; // reserved for future cross-module hook
    const vcpuRaw = Number(env("SANDBOX_VCPUS") ?? "2");
    const vcpus = Number.isFinite(vcpuRaw) && vcpuRaw >= 1 ? Math.min(8, vcpuRaw) : 2;
    const sb = await Sandbox.getOrCreate({
      name: SANDBOX_NAME,
      runtime: "node24",
      persistent: true,
      timeout: 60 * 60 * 1000,
      resources: { vcpus },
      env: {
        OPENAI_API_KEY: env("OPENAI_API_KEY") ?? "",
        ANTHROPIC_API_KEY: env("ANTHROPIC_API_KEY") ?? "",
      },
    });
    return sb;
  })();
  return sandboxPromise;
}

export { chooseEngine, hasClaudeAuth };

// Snapshot a tenant's coding-agent state (auth + session history) into a
// dict so we can stash it in Redis and restore it on a cold sandbox. Best
// effort — missing files just yield an empty snapshot.
//
// Why this captures more than the per-tenant config dir:
//   - Claude Code keeps OAuth/config in CLAUDE_CONFIG_DIR (per-tenant), but
//     its `--continue` session log lives at `${workdir}/.claude/projects/…`
//     for the project being worked on. Snapshotting only the config dir
//     means every /code attach turn starts fresh, with no memory.
//   - opencode stores auth in `~/.opencode/auth.json` (HOME-relative, not
//     per-tenant) and its session state in `${workdir}/.opencode/`. The
//     previous code captured neither — so on a cold sandbox, opencode lost
//     both its OpenAI cred AND its session.
//
// Strategy: enumerate a small fixed set of "interesting" roots (config dir,
// workdir/.claude, workdir/.opencode, ~/.opencode) and snapshot every file
// under each, keyed by an absolute-path string so we can replay verbatim.
export async function snapshotClaudeAuth(
  tenantId: string,
  workdir?: string
): Promise<Record<string, string>> {
  let sandbox: Sandbox;
  try {
    sandbox = await getSharedSandbox();
  } catch {
    return {};
  }
  const cfgDir = configDirFor(tenantId);
  const roots = [cfgDir, "/root/.opencode"];
  if (workdir) {
    roots.push(`${workdir}/.claude`, `${workdir}/.opencode`);
  }

  const snap: Record<string, string> = {};
  for (const root of roots) {
    try {
      const ls = await sandbox.runCommand("sh", [
        "-c",
        // Bound depth + file count so a wild session log doesn't blow up
        // Redis. 80 files at 64KB each = ~5MB worst case per tenant.
        `find ${shellQuote(root)} -maxdepth 6 -type f 2>/dev/null | head -80`,
      ]);
      const out = (await ls.stdout()).trim();
      if (!out) continue;
      const files = out.split("\n").map((s) => s.trim()).filter(Boolean);
      for (const path of files) {
        const sizeCheck = await sandbox.runCommand("sh", [
          "-c",
          `stat -c%s ${shellQuote(path)} 2>/dev/null`,
        ]);
        const sizeStr = (await sizeCheck.stdout()).trim();
        const size = Number.parseInt(sizeStr, 10);
        if (!Number.isFinite(size) || size > 64 * 1024) continue;
        const r = await sandbox.runCommand("sh", [
          "-c",
          `cat ${shellQuote(path)} 2>/dev/null | base64`,
        ]);
        const b64 = (await r.stdout()).trim();
        if (b64) {
          // Key by absolute path — restore writes back to the same place,
          // which makes the snapshot trivially location-stable across the
          // engine/workdir mix.
          snap[path] = b64;
        }
      }
    } catch {
      // Skip this root; keep going with the others.
    }
  }
  return snap;
}

// Restore a previously-captured snapshot into the sandbox. Used on cold
// sandbox restart to bring claude/opencode session history + auth back.
//
// `tenantId` and `workdir` are accepted but no longer used directly — keys
// in the snapshot are absolute paths, so we just recreate the directory
// tree and base64-decode each file in place. The parameters stay in the
// signature for call-site clarity and forward compatibility (e.g. if we
// ever need to rewrite paths during cross-tenant moves).
export async function restoreClaudeAuth(
  tenantId: string,
  snapshot: Record<string, string>,
  workdir?: string
): Promise<void> {
  void tenantId;
  void workdir;
  if (!snapshot || Object.keys(snapshot).length === 0) return;
  let sandbox: Sandbox;
  try {
    sandbox = await getSharedSandbox();
  } catch {
    return;
  }
  try {
    for (const [dest, b64] of Object.entries(snapshot)) {
      // Defensive: ignore non-absolute keys (legacy snapshots used
      // tenant-config-dir-relative keys). They land outside the sandbox
      // root which the sandbox shell wouldn't write to anyway.
      if (!dest.startsWith("/")) continue;
      const destDir = dest.split("/").slice(0, -1).join("/");
      const restoreCmd =
        `mkdir -p ${shellQuote(destDir)} && ` +
        `printf %s ${shellQuote(b64)} | base64 -d > ${shellQuote(dest)}`;
      await sandbox.runCommand("sh", ["-c", restoreCmd]);
    }
  } catch {
    // best-effort
  }
}

export async function runClaudeCode(
  req: ClaudeCodeRequest
): Promise<ClaudeCodeResult> {
  const engine = req.forceEngine ?? chooseEngine();
  if (engine === "none") {
    return {
      ok: false,
      output: "",
      exitCode: -1,
      workdir: "",
      error:
        "No coding-agent auth configured. Set ANTHROPIC_API_KEY (preferred), AI_GATEWAY_API_KEY, or OPENAI_API_KEY (falls back to OpenCode with gpt-5.3-codex) in Vercel env.",
    };
  }

  const progress = async (phase: string) => {
    try {
      if (req.onProgress) await req.onProgress(phase);
    } catch {
      // progress reporting failures are non-fatal
    }
  };

  await progress("preparing sandbox");

  let sandbox: Sandbox;
  try {
    sandbox = await getSharedSandbox();
  } catch (err: any) {
    return {
      ok: false,
      output: "",
      exitCode: -1,
      workdir: "",
      error: `sandbox unavailable: ${err?.message ?? String(err)}`,
    };
  }

  const project = req.project ?? "default";
  const workdir = req.absoluteWorkdir ?? workdirFor(req.tenantId, project);
  const cfgDir = configDirFor(req.tenantId);

  // Make the workdirs (no-op if they already exist).
  try {
    await sandbox.runCommand("sh", [
      "-c",
      `mkdir -p ${workdir} ${cfgDir} 2>&1`,
    ]);
  } catch (err: any) {
    return {
      ok: false,
      output: "",
      exitCode: -1,
      workdir,
      error: `mkdir workdir failed: ${err?.message ?? String(err)}`,
    };
  }

  // Repo bootstrap (optional). When req.repoUrl is set we clone (or pull) the
  // repo into the workdir before invoking Claude, so Claude operates on a
  // real codebase rather than an empty scratch dir.
  let githubTokenForPush: string | null = null;
  if (req.repoUrl) {
    const conn = await getGithubTokenForTenant(req.tenantId);
    if (!conn) {
      return {
        ok: false,
        output: "",
        exitCode: -1,
        workdir,
        error:
          "GitHub not connected for this user. Authorize GitHub in the Composio dashboard, then retry. (Composio toolkit: github)",
      };
    }
    githubTokenForPush = conn.token;

    const authedUrl = buildAuthedGitUrl(req.repoUrl, conn.token);
    if (!authedUrl) {
      return {
        ok: false,
        output: "",
        exitCode: -1,
        workdir,
        error: `invalid repoUrl: ${req.repoUrl}`,
      };
    }

    // skipClone mode: the workdir already has the project's files
    // (e.g. from a prior /code turn), and the target repo is brand new
    // and empty so there's nothing to clone. Initialize the workdir as
    // a git repo (if not already), wire the target as `origin`, and
    // fall through to the engine run + push at the bottom. Used by the
    // agent's "publish this /code project to a new GitHub repo" chain:
    //   1. composio_execute_tool GITHUB_CREATE_REPO  → new repo URL
    //   2. ask_claude_code with skip_clone=true, repo_url=<new>,
    //      push_to_branch=<branch>  → wires up + commits + pushes.
    if (req.skipClone) {
      await progress(`initializing ${req.repoUrl}`);
      const setupCmd =
        `cd ${shellQuote(workdir)} && ` +
        // Init iff .git is missing. The default branch name matches the
        // common convention for fresh GitHub repos so the push doesn't
        // implicitly create a `master` branch on a `main`-named repo.
        `( [ -d .git ] || git init -q -b main ) && ` +
        // Ensure git identity so commit doesn't trip "tell me who you are".
        `git config user.email "agentos+${shellQuote(sanitizeTenantId(req.tenantId))}@noreply.agentos" && ` +
        `git config user.name "agentOS (${shellQuote(sanitizeTenantId(req.tenantId))})" && ` +
        // Add or update origin idempotently — `git remote add` errors on
        // a second invocation, so try add and fall back to set-url.
        `( git remote add origin ${shellQuote(authedUrl)} 2>/dev/null || ` +
        `  git remote set-url origin ${shellQuote(authedUrl)} ) 2>&1`;
      const setup = await sandbox.runCommand("sh", ["-c", setupCmd]);
      if (setup.exitCode !== 0) {
        const out = await setup.stdout();
        const err = await setup.stderr();
        return {
          ok: false,
          output: out || err,
          exitCode: setup.exitCode,
          workdir,
          error: `git init/add-origin failed (exit ${setup.exitCode}): ${(err || out).slice(-400)}`,
        };
      }
      // Skip the clone-or-fetch block below — proceed straight to the
      // engine run + push.
    } else {

    await progress(`cloning ${req.repoUrl}`);

    // Clone-or-pull, with three branches because `git clone <url> .` requires
    // an empty destination and the workdir can legitimately be in any of
    // three states:
    //
    //   1. Has .git for some repo                → fetch (already cloned, or
    //                                              a different repo we just
    //                                              switch the origin on).
    //   2. Genuinely empty                       → plain clone.
    //   3. Has stuff but no .git                 → wipe + clone. This branch
    //                                              previously didn't exist —
    //                                              git would fail with
    //                                              "destination path '.'
    //                                              already exists and is not
    //                                              an empty directory" and
    //                                              the user-facing agent
    //                                              would surface a confusing
    //                                              "workspace isn't empty"
    //                                              snag. Common causes: a
    //                                              prior turn left a prompt
    //                                              file or a restored
    //                                              session log under
    //                                              `.claude/`, the previous
    //                                              clone got SIGKILL'd
    //                                              between fetching objects
    //                                              and writing .git, etc.
    //                                              The workdir is project-
    //                                              scratch; nothing in it is
    //                                              load-bearing for a fresh
    //                                              clone.
    const cloneCmd =
      `cd ${shellQuote(workdir)} && ` +
      `if [ -d .git ]; then ` +
      `  git remote set-url origin ${shellQuote(authedUrl)} && ` +
      `  git fetch --all --prune 2>&1; ` +
      `elif [ -z "$(ls -A 2>/dev/null)" ]; then ` +
      `  git clone --depth 50 ${shellQuote(authedUrl)} . 2>&1; ` +
      `else ` +
      `  echo "workdir not empty and no .git — clearing for fresh clone" >&2 && ` +
      `  find . -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>&1 && ` +
      `  git clone --depth 50 ${shellQuote(authedUrl)} . 2>&1; ` +
      `fi`;
    const clone = await sandbox.runCommand("sh", ["-c", cloneCmd]);
    const cloneStdout = await clone.stdout();
    const cloneStderr = await clone.stderr();
    if (clone.exitCode !== 0) {
      return {
        ok: false,
        output: cloneStdout || cloneStderr,
        exitCode: clone.exitCode,
        workdir,
        error: `git clone/fetch failed (exit ${clone.exitCode}): ${(cloneStderr || cloneStdout).slice(-400)}`,
      };
    }

    // Configure git identity so commit/push later don't trip "please tell me
    // who you are". Best-effort.
    await sandbox.runCommand("sh", [
      "-c",
      `cd ${shellQuote(workdir)} && ` +
        `git config user.email "agentos+${shellQuote(sanitizeTenantId(req.tenantId))}@noreply.agentos" && ` +
        `git config user.name "agentOS (${shellQuote(sanitizeTenantId(req.tenantId))})" 2>&1 || true`,
    ]);

    // Check out base branch if requested.
    if (req.baseBranch) {
      const checkout = await sandbox.runCommand("sh", [
        "-c",
        `cd ${shellQuote(workdir)} && git checkout ${shellQuote(req.baseBranch)} 2>&1`,
      ]);
      if (checkout.exitCode !== 0) {
        const err = await checkout.stderr();
        return {
          ok: false,
          output: "",
          exitCode: checkout.exitCode,
          workdir,
          error: `git checkout ${req.baseBranch} failed: ${err.slice(-300)}`,
        };
      }
    }
    } // end else (clone branch)
  }

  // Write the user prompt to a file we feed via stdin. Embedding it in the
  // command line would break on quoting and on very long prompts.
  const promptFile = `${workdir}/.__cc_prompt`;
  await sandbox.writeFiles([
    { path: promptFile, content: Buffer.from(req.prompt, "utf8") },
  ]);

  // Build the shell command per chosen engine.
  let cmdShell: string;
  if (engine === "claude") {
    const model = req.model ?? env("CLAUDE_CODE_MODEL") ?? "sonnet";
    const claudeEnv = buildClaudeEnv(req.tenantId);

    // --print = non-interactive single-turn.
    // --dangerously-skip-permissions = auto-approve every tool call (the
    // user explicitly opted in; documented in the tool description).
    // --continue = resume the most recent session in this workdir's history.
    const claudeArgs: string[] = ["--print", "--dangerously-skip-permissions"];
    if (req.continueSession) claudeArgs.push("--continue");
    if (model) claudeArgs.push("--model", model);

    const envExports = Object.entries(claudeEnv)
      .map(([k, v]) => `${k}=${shellQuote(v)}`)
      .join(" ");
    cmdShell =
      `cd ${shellQuote(workdir)} && ` +
      `${envExports} claude ${claudeArgs.map(shellQuote).join(" ")} < ${shellQuote(promptFile)} 2>&1`;
  } else {
    // OpenCode fallback. Install lazily — sandboxes provisioned before this
    // slice don't have opencode pre-installed, and bumping the install
    // marker would force a full reinstall of everything. `which opencode`
    // skips the cost on warm runs.
    const install = await sandbox.runCommand("sh", [
      "-c",
      `which opencode >/dev/null 2>&1 || npm install -g opencode-ai 2>&1`,
    ]);
    if (install.exitCode !== 0) {
      const out = await install.stdout();
      const errx = await install.stderr();
      return {
        ok: false,
        output: "",
        exitCode: install.exitCode,
        workdir,
        error: `opencode install failed (exit ${install.exitCode}): ${(errx || out).slice(-400)}`,
      };
    }

    // Authenticate OpenCode against OpenAI. The CLI persists creds under
    // ~/.opencode/, so we only do this once per sandbox; subsequent calls
    // are a no-op (`opencode auth add` is idempotent).
    const openaiKey = env("OPENAI_API_KEY") ?? "";
    await sandbox.runCommand("sh", [
      "-c",
      `printf %s ${shellQuote(openaiKey)} | opencode auth add openai >/dev/null 2>&1 || true`,
    ]);

    // OpenCode is auto-non-interactive in `run` mode; no equivalent of
    // --dangerously-skip-permissions flag needed.
    const primaryModel = req.model
      ? `openai/${req.model}`
      : opencodeModel();
    const fallback = opencodeFallbackModel();

    // Try primary, fall back to alt model on non-zero exit. Both runs use
    // the same prompt file. --continue resumes the latest session in this
    // workdir (mirrors claude --continue semantics).
    const continueFlag = req.continueSession ? " --continue" : "";
    const tryPrimary =
      `cd ${shellQuote(workdir)} && ` +
      `OPENAI_API_KEY=${shellQuote(openaiKey)} ` +
      `opencode run --model ${shellQuote(primaryModel)}${continueFlag} ` +
      `"$(cat ${shellQuote(promptFile)})" 2>&1`;
    const tryFallback =
      `cd ${shellQuote(workdir)} && ` +
      `OPENAI_API_KEY=${shellQuote(openaiKey)} ` +
      `opencode run --model ${shellQuote(fallback)}${continueFlag} ` +
      `"$(cat ${shellQuote(promptFile)})" 2>&1`;
    cmdShell = `(${tryPrimary}) || (${tryFallback})`;
  }

  const timeout = req.timeoutMs ?? DEFAULT_TIMEOUT_MS;

  await progress(`engine running (${engine})`);

  // Detached-mode fast path. Used by long-running /code turns that
  // must survive Vercel function-instance teardown — the engine keeps
  // running in the sandbox after we return, and the caller polls via
  // awaitClaudeCommand on subsequent function invocations.
  //
  // We do NOT use Vercel Sandbox's `runCommand({ detached: true })`
  // + `sandbox.getCommand(cmdId)` here. That pattern hits HTTP 410
  // ("Gone") when the sandbox GCs the command record — Vercel doesn't
  // guarantee a long retention window for detached commands, so any
  // turn that takes more than a few minutes would 410 the moment we
  // tried to rehydrate. Instead we shell out via a wrapper script
  // that:
  //
  //   1. backgrounds the actual engine (`nohup … &`) so the engine
  //      process survives the foreground shell that spawned it,
  //   2. redirects stdout+stderr to `${workdir}/.__cc_<id>.log`,
  //   3. writes the engine's exit code to `${workdir}/.__cc_<id>.exit`
  //      when the engine finishes.
  //
  // Polling is then just `test -f .exit` (done) and `cat .log` (output).
  // The "id" is what we hand back as the cmdId — purely our own string,
  // not a Vercel handle, so it cannot be GC'd out from under us.
  if (req.detached) {
    // Hybrid pattern: dispatch the engine via Vercel Sandbox's native
    // `detached: true` mode (which we know returns in milliseconds and
    // doesn't hang sandbox.runCommand on backgrounded children), BUT
    // route the engine's output through filesystem markers so polling
    // can use plain `test -f` + `cat` instead of `sandbox.getCommand`
    // + `cmd.wait()`. The latter pair hits HTTP 410 after Vercel GCs
    // the command record; the file markers survive as long as the
    // sandbox VM does.
    const runId =
      "t" + Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
    const logFile = `${workdir}/.__cc_${runId}.log`;
    const exitFile = `${workdir}/.__cc_${runId}.exit`;

    // Wrap the engine command so its full output lands in logFile and
    // its exit code in exitFile when it finishes. cmdShell already
    // ends with `2>&1` internally, but the outer redirect makes that
    // a no-op — we want both streams in the log either way.
    const detachedShell =
      `: > ${shellQuote(logFile)}; ` +
      `( ${cmdShell} ) >> ${shellQuote(logFile)} 2>&1; ` +
      `echo $? > ${shellQuote(exitFile)}`;

    try {
      await (sandbox as any).runCommand({
        cmd: "sh",
        args: ["-c", detachedShell],
        detached: true,
      });
      return {
        ok: true,
        output: "",
        exitCode: -1,
        workdir,
        // Encode the runId AND the workdir so the await side can find
        // the marker files without re-loading project meta. Note that
        // we no longer carry Vercel's cmdId — we don't need it.
        error: `__cmdId:${runId}|${workdir}`,
      };
    } catch (err: any) {
      return {
        ok: false,
        output: "",
        exitCode: -1,
        workdir,
        error: `engine background launch threw: ${err?.message ?? String(err)}`,
      };
    }
  }

  let cmd;
  try {
    cmd = await sandbox.runCommand({
      cmd: "sh",
      args: ["-c", cmdShell],
    } as any);
  } catch (err: any) {
    return {
      ok: false,
      output: "",
      exitCode: -1,
      workdir,
      error: `runCommand failed: ${err?.message ?? String(err)}`,
    };
  }

  // Honor req.timeoutMs as a real wall-clock deadline (was previously
  // informational — see the deleted `void timeout;` line below). On expiry,
  // best-effort kill the running command so it stops burning sandbox budget,
  // then surface a clear error so the codeWorkflow records the turn as
  // failed and bumps turnCount (so /code attach isn't blocked by a zombie
  // pending state).
  let stdout: string;
  let stderr: string;
  try {
    stdout = await withDeadline(cmd.stdout(), timeout, "engine stdout");
    stderr = await withDeadline(cmd.stderr(), timeout, "engine stderr");
  } catch (e: any) {
    const timeoutMsg = e?.message ?? String(e);
    try {
      const k = (cmd as any).kill;
      if (typeof k === "function") await k.call(cmd);
    } catch {
      // best-effort kill; sandbox will reap on its own timeout
    }
    return {
      ok: false,
      output: "",
      exitCode: -1,
      workdir,
      error: `engine timed out (${timeoutMsg}). Increase timeoutMs or check engine auth/network inside the sandbox.`,
    };
  }
  const output = stdout || stderr;

  // Best-effort session log hint: claude writes session JSON files under
  // CLAUDE_CONFIG_DIR/projects. We expose the path so future calls can
  // `--continue` against it.
  let sessionLogHint: string | undefined;
  try {
    const ls = await sandbox.runCommand("sh", [
      "-c",
      `ls -t ${cfgDir}/projects 2>/dev/null | head -1`,
    ]);
    const top = (await ls.stdout()).trim();
    if (top) sessionLogHint = `${cfgDir}/projects/${top}`;
  } catch {
    // ignore
  }

  if (cmd.exitCode !== 0) {
    return {
      ok: false,
      output,
      exitCode: cmd.exitCode,
      workdir,
      sessionLogHint,
      error: `claude exited ${cmd.exitCode}: ${(stderr || stdout).slice(-400)}`,
    };
  }

  // Optional push step (repo flow). If Claude succeeded and the caller asked
  // for pushToBranch, create that branch, stage everything, commit, push.
  // Failures here surface as repoPush.ok=false but don't fail the whole
  // call — Claude's work is still useful even if push fails.
  let repoPush: ClaudeCodeResult["repoPush"];
  if (req.repoUrl && req.pushToBranch && githubTokenForPush) {
    const commitMsg =
      req.commitMessage ??
      `agentOS: ${req.prompt.slice(0, 60).replace(/\s+/g, " ")}`;
    const branch = req.pushToBranch;

    const pushCmd =
      `cd ${shellQuote(workdir)} && ` +
      // Stage, but tolerate empty diffs.
      `git add -A 2>&1 && ` +
      // Create or switch to the branch.
      `(git checkout -B ${shellQuote(branch)} 2>&1) && ` +
      // Commit; --allow-empty so a no-op task still produces a marker commit
      // the caller can detect.
      `git commit -m ${shellQuote(commitMsg)} --allow-empty 2>&1 && ` +
      `git push -u origin ${shellQuote(branch)} 2>&1`;
    const push = await sandbox.runCommand("sh", ["-c", pushCmd]);
    const pushStdout = await push.stdout();
    const pushStderr = await push.stderr();
    if (push.exitCode === 0) {
      repoPush = {
        attempted: true,
        ok: true,
        branch,
        commitMessage: commitMsg,
      };
    } else {
      repoPush = {
        attempted: true,
        ok: false,
        branch,
        commitMessage: commitMsg,
        error: (pushStderr || pushStdout).slice(-400),
      };
    }
  }

  return {
    ok: true,
    output: output.trim(),
    exitCode: 0,
    workdir,
    sessionLogHint,
    ...(repoPush ? { repoPush } : {}),
  };
}

// File-based poll for a detached engine. Each call:
//
//   1. Resolves the workdir + log/exit-marker paths from `cmdId`
//      (which is "<runId>|<workdir>" — see runClaudeCode's detached
//      branch for the encoding).
//   2. Checks whether the exit marker file exists. If yes, the engine
//      has finished — we read its exit code and full output.
//   3. If not, returns `{ done: false }` so the workflow loops onto a
//      fresh function invocation. The engine keeps running in the
//      sandbox the entire time.
//
// Polling is cheap (one or two short shell commands per call), so the
// workflow's inter-poll cadence is dominated by step boundaries, not
// our deadline. The wait pattern that used to live here
// (`cmd.wait({signal})`) was abandoned because Vercel's API GCs
// detached command records, returning HTTP 410 after a few minutes.
export async function awaitClaudeCommand(args: {
  cmdId: string;
}): Promise<ClaudeCodeFinishResult> {
  // The cmdId encodes "<runId>|<workdir>" so we don't need to reload
  // the project meta from Redis on every poll.
  const sep = args.cmdId.indexOf("|");
  if (sep < 1) {
    return {
      done: true,
      ok: false,
      output: "",
      exitCode: -1,
      error: `malformed cmdId (missing workdir): ${args.cmdId}`,
    };
  }
  const runId = args.cmdId.slice(0, sep);
  const workdir = args.cmdId.slice(sep + 1);
  const logFile = `${workdir}/.__cc_${runId}.log`;
  const exitFile = `${workdir}/.__cc_${runId}.exit`;

  let sandbox: Sandbox;
  try {
    sandbox = await getSharedSandbox();
  } catch (err: any) {
    return {
      done: true,
      ok: false,
      output: "",
      exitCode: -1,
      error: `sandbox unavailable while polling ${runId}: ${err?.message ?? String(err)}`,
    };
  }

  // Single shell that does both checks at once — exit code = 0 if the
  // engine has finished, non-zero if still running. Output on stdout is
  // the exit code value followed by a newline followed by the log
  // contents; on the "not done" path stdout is just the literal "running".
  const check = await sandbox.runCommand("sh", [
    "-c",
    `if [ -f ${shellQuote(exitFile)} ]; then ` +
      `  cat ${shellQuote(exitFile)}; ` +
      `  echo "----LOG----"; ` +
      `  cat ${shellQuote(logFile)} 2>/dev/null; ` +
      `else ` +
      `  echo running; ` +
      `fi`,
  ]);
  const raw = (await check.stdout()).toString();

  if (raw.startsWith("running")) {
    return { done: false };
  }

  // Done: parse exit code and log.
  const idx = raw.indexOf("----LOG----");
  const codeRaw = (idx >= 0 ? raw.slice(0, idx) : raw).trim();
  const logBody = idx >= 0 ? raw.slice(idx + "----LOG----".length).replace(/^\n/, "") : "";
  const exitCode = Number.parseInt(codeRaw, 10);
  const ok = Number.isFinite(exitCode) && exitCode === 0;
  return {
    done: true,
    ok,
    output: logBody.trim(),
    exitCode: Number.isFinite(exitCode) ? exitCode : -1,
    error: ok
      ? undefined
      : `engine exited ${codeRaw}: ${logBody.slice(-400)}`,
  };
}

// Build a git URL with the token embedded for HTTPS auth. Only mutates
// github.com URLs; returns null if the URL doesn't look like a git remote.
function buildAuthedGitUrl(repoUrl: string, token: string): string | null {
  try {
    const u = new URL(repoUrl);
    if (u.protocol !== "https:") return null;
    // GitHub accepts "x-access-token:<TOKEN>@github.com" for OAuth tokens.
    u.username = "x-access-token";
    u.password = token;
    return u.toString();
  } catch {
    return null;
  }
}

// Minimal shell quoter — strict single-quote wrap, escape embedded quotes.
// Faster than spawning bash -c with array args because the env-export prefix
// needs to be one command line.
function shellQuote(s: string): string {
  if (s === "") return "''";
  if (/^[A-Za-z0-9._\/+@:=,-]+$/.test(s)) return s;
  return "'" + s.replace(/'/g, "'\\''") + "'";
}
