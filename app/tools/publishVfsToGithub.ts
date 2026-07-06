// app/tools/publishVfsToGithub.ts
//
// Direct VFS→GitHub publish path: reads files from the tenant's Redis-
// backed VFS and uploads each one to a target GitHub repo using the
// tenant's Composio-managed GitHub OAuth token. Skips the chain through
// composio_execute_tool + GITHUB_CREATE_OR_UPDATE_FILE_CONTENTS because
// that path requires the agent to make N+1 tool calls (1 schema lookup,
// then 1 per file). This tool does the whole batch in one call so the
// agent doesn't need to loop, and so the user's "summary in VFS → push
// to GitHub" flow is one-shot.

import { tool } from "ai";
import { z } from "zod";

import { getStore } from "@/app/lib/store";
import { getGithubTokenForTenant } from "@/app/lib/composioGithub";
import { recordAudit } from "@/app/lib/auditLog";

export type PublishVfsToGithubContext = {
  tenantId: string;
  sessionId: string;
};

type VfsFileNode = {
  type: "file";
  path: string;
  content: string;
};

function sanitizePath(input: string): string {
  let p = String(input ?? "").trim();
  if (!p) p = "/workspace";
  if (!p.startsWith("/")) p = `/workspace/${p}`;
  p = p.replace(/\/+/g, "/");
  const parts = p.split("/");
  const out: string[] = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (out.length > 0) out.pop();
      continue;
    }
    out.push(part);
  }
  return `/${out.join("/")}`;
}

// Resolve a list of VFS paths from either an explicit `paths` array or
// a `prefix` (in which case we enumerate every member of the tenant's
// vfs:{tenant}:{session}:paths SET that starts with the prefix).
async function resolvePaths(args: {
  tenantId: string;
  sessionId: string;
  paths?: string[];
  prefix?: string;
}): Promise<string[]> {
  if (Array.isArray(args.paths) && args.paths.length > 0) {
    return args.paths.map(sanitizePath);
  }
  if (typeof args.prefix === "string" && args.prefix) {
    const prefix = sanitizePath(args.prefix);
    const all = await getStore().smembers(
      `vfs:${args.tenantId}:${args.sessionId}:paths`
    );
    return all.filter((p) => p === prefix || p.startsWith(prefix + "/"));
  }
  return [];
}

async function readVfsFile(args: {
  tenantId: string;
  sessionId: string;
  path: string;
}): Promise<VfsFileNode | null> {
  const node = await getStore().get<VfsFileNode | { type: "dir" }>(
    `vfs:${args.tenantId}:${args.sessionId}:node:${sanitizePath(args.path)}`
  );
  if (!node || node.type !== "file") return null;
  return node;
}

// Parse a GitHub URL (https://github.com/owner/repo or owner/repo) into
// its parts. Returns null on unparseable input.
function parseRepoUrl(input: string): { owner: string; repo: string } | null {
  const m = String(input ?? "")
    .trim()
    .match(/^(?:https?:\/\/github\.com\/)?([A-Za-z0-9._-]+)\/([A-Za-z0-9._-]+?)(?:\.git)?$/);
  if (!m) return null;
  return { owner: m[1]!, repo: m[2]! };
}

// Strip the VFS workspace prefix and any user-specified strip so the
// in-repo path is e.g. `summary.md` instead of `/workspace/job-output/summary.md`.
function vfsToRepoPath(vfsPath: string, opts?: { strip?: string }): string {
  let p = vfsPath.replace(/^\/+/, "");
  // Default: strip the leading `workspace/` segment that every VFS path has.
  if (p.startsWith("workspace/")) p = p.slice("workspace/".length);
  if (opts?.strip) {
    const s = opts.strip.replace(/^\/+/, "").replace(/\/+$/, "");
    if (s && p.startsWith(s + "/")) p = p.slice(s.length + 1);
    else if (s && p === s) p = "";
  }
  return p;
}

// One file → GitHub Contents API PUT. Handles "file exists, need sha
// for update" the way GitHub requires.
async function putFileViaGithubContentsApi(args: {
  token: string;
  owner: string;
  repo: string;
  branch: string;
  pathInRepo: string;
  content: string;
  commitMessage: string;
}): Promise<{ ok: boolean; status: number; sha?: string; error?: string }> {
  // Step 1: probe whether the file already exists on the target branch
  // — if it does, GitHub requires the existing blob sha for an update.
  let existingSha: string | undefined;
  try {
    const probe = await fetch(
      `https://api.github.com/repos/${args.owner}/${args.repo}/contents/${encodeURI(args.pathInRepo)}?ref=${encodeURIComponent(args.branch)}`,
      {
        headers: {
          Authorization: `token ${args.token}`,
          Accept: "application/vnd.github+json",
          "User-Agent": "agentos",
        },
      }
    );
    if (probe.ok) {
      const body = (await probe.json()) as { sha?: string };
      existingSha = body.sha;
    }
  } catch {
    // probe failure is non-fatal; PUT below will surface the real error
  }

  // Step 2: PUT the new contents. Base64-encode the body (GitHub's API
  // requires base64 even for text files).
  const b64 = Buffer.from(args.content, "utf8").toString("base64");
  const res = await fetch(
    `https://api.github.com/repos/${args.owner}/${args.repo}/contents/${encodeURI(args.pathInRepo)}`,
    {
      method: "PUT",
      headers: {
        Authorization: `token ${args.token}`,
        Accept: "application/vnd.github+json",
        "Content-Type": "application/json",
        "User-Agent": "agentos",
      },
      body: JSON.stringify({
        message: args.commitMessage,
        content: b64,
        branch: args.branch,
        ...(existingSha ? { sha: existingSha } : {}),
      }),
    }
  );
  if (res.ok) {
    const body = (await res.json()) as { content?: { sha?: string } };
    return { ok: true, status: res.status, sha: body.content?.sha };
  }
  const errText = await res.text().catch(() => "");
  return {
    ok: false,
    status: res.status,
    error: `${res.status}: ${errText.slice(0, 240)}`,
  };
}

export function makePublishVfsToGithubTool(ctx: PublishVfsToGithubContext) {
  return tool({
    description: [
      "Publish files from the tenant's VFS (the Redis-backed virtual",
      "filesystem the agent writes to) directly to a GitHub repository,",
      "using the user's Composio-managed GitHub OAuth token. One call,",
      "multiple files, single commit per file.",
      "",
      "Use this when:",
      "  - The user asks to push, publish, ship, or commit files from",
      "    the VFS to GitHub (e.g. \"save the summary to GitHub\",",
      "    \"publish /workspace/deep-jobs/p_abc/ to github.com/me/notes\").",
      "  - You've written one or more outputs to the VFS during the",
      "    current job/turn and the user wants them in a real repo.",
      "  - The user uploaded a draft and wants it committed.",
      "",
      "Do NOT use this for:",
      "  - Long-running code-edit sessions — those still go through",
      "    /code or ask_claude_code, which runs claude/opencode in a",
      "    sandbox against the cloned repo.",
      "  - Pushing files that don't exist in the VFS yet — write them",
      "    first via write_virtual_file or use a tool that does.",
      "",
      "Auth: the tenant's GitHub must be connected through Composio.",
      "If it isn't, this tool returns a structured error telling you",
      "to ask the user to authorize GitHub in Composio. Do NOT ask",
      "the user for a personal access token.",
      "",
      "Path mapping: by default the leading `workspace/` is stripped",
      "from each VFS path so `/workspace/summary.md` becomes",
      "`summary.md` in the repo. Use `path_strip` to remove a longer",
      "prefix (e.g. `path_strip: \"deep-jobs/p_abc\"` to flatten a",
      "job-output directory).",
    ].join("\n"),
    inputSchema: z.object({
      repo_url: z
        .string()
        .describe(
          "Target GitHub repo. Accepts `https://github.com/owner/repo` or just `owner/repo`."
        ),
      branch: z
        .string()
        .nullable()
        .describe(
          "Branch to commit to. If null, defaults to `main`. The branch must already exist on the remote — this tool does not create branches."
        ),
      paths: z
        .array(z.string())
        .nullable()
        .describe(
          "Explicit list of VFS paths to publish. Either this OR `prefix` must be set. Paths are sanitized and resolved against the tenant's VFS."
        ),
      prefix: z
        .string()
        .nullable()
        .describe(
          "Publish every file in the VFS under this path prefix. Example: `/workspace/deep-jobs/j_abc/` publishes the whole job output dir. Either this OR `paths` must be set."
        ),
      commit_message: z
        .string()
        .nullable()
        .describe(
          "Commit message used for each file's commit. Defaults to a sensible agentOS marker."
        ),
      path_strip: z
        .string()
        .nullable()
        .describe(
          "Strip this prefix from each VFS path before mapping to repo path. The leading `workspace/` is always stripped automatically; this is for stripping further sub-prefixes."
        ),
    }),
    execute: async (args) => {
      const parsedRepo = parseRepoUrl(args.repo_url);
      if (!parsedRepo) {
        return {
          ok: false,
          error: `Couldn't parse repo URL: ${args.repo_url}. Expected https://github.com/owner/repo or owner/repo.`,
        };
      }
      const branch = args.branch ?? "main";
      const commitMessage =
        args.commit_message ??
        `agentOS: publish ${parsedRepo.repo} files from VFS`;

      const githubConn = await getGithubTokenForTenant(ctx.tenantId);
      if (!githubConn) {
        return {
          ok: false,
          error:
            "GitHub is not connected for this user in Composio. Ask the user to authorize the GitHub toolkit in their Composio dashboard, then retry. Do not ask for a personal access token.",
        };
      }

      const vfsPaths = await resolvePaths({
        tenantId: ctx.tenantId,
        sessionId: ctx.sessionId,
        paths: args.paths ?? undefined,
        prefix: args.prefix ?? undefined,
      });
      if (vfsPaths.length === 0) {
        return {
          ok: false,
          error:
            "No VFS files matched. Provide `paths` (array) or `prefix` (string). Check spelling: paths must start with /workspace/.",
        };
      }

      const published: Array<{ vfs: string; repo: string; sha?: string }> = [];
      const failed: Array<{ vfs: string; error: string }> = [];
      for (const vfsPath of vfsPaths) {
        const node = await readVfsFile({
          tenantId: ctx.tenantId,
          sessionId: ctx.sessionId,
          path: vfsPath,
        });
        if (!node) {
          failed.push({ vfs: vfsPath, error: "not a file in VFS (skipped)" });
          continue;
        }
        const pathInRepo = vfsToRepoPath(vfsPath, {
          strip: args.path_strip ?? undefined,
        });
        if (!pathInRepo) {
          failed.push({ vfs: vfsPath, error: "empty repo path after strip" });
          continue;
        }
        const result = await putFileViaGithubContentsApi({
          token: githubConn.token,
          owner: parsedRepo.owner,
          repo: parsedRepo.repo,
          branch,
          pathInRepo,
          content: node.content,
          commitMessage,
        });
        if (result.ok) {
          published.push({ vfs: vfsPath, repo: pathInRepo, sha: result.sha });
        } else {
          failed.push({ vfs: vfsPath, error: result.error ?? `status ${result.status}` });
        }
      }

      // Best-effort audit emit so the /ui Activity panel surfaces the
      // publish event alongside other code/job actions.
      try {
        await recordAudit(ctx.tenantId, {
          kind: failed.length === 0 ? "tool.code_push" : "tool.code_push",
          summary:
            failed.length === 0
              ? `Published ${published.length} file(s) to ${parsedRepo.owner}/${parsedRepo.repo} (${branch})`
              : `Partial publish to ${parsedRepo.owner}/${parsedRepo.repo} (${branch}): ${published.length} ok, ${failed.length} failed`,
          meta: {
            repo: `${parsedRepo.owner}/${parsedRepo.repo}`,
            branch,
            published: published.map((p) => p.repo),
            failed: failed.map((f) => f.vfs),
          },
        });
      } catch {
        // best-effort
      }

      return {
        ok: failed.length === 0,
        repo: `${parsedRepo.owner}/${parsedRepo.repo}`,
        branch,
        published_count: published.length,
        failed_count: failed.length,
        published,
        failed,
        commit_url:
          published.length > 0
            ? `https://github.com/${parsedRepo.owner}/${parsedRepo.repo}/tree/${branch}`
            : undefined,
      };
    },
  });
}
