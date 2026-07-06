// app/steps/codeSteps.ts
//
// Durable WDK steps that drive a long-running /code project. Each function
// is `"use step"` so the workflow log persists across function-instance
// boundaries and the project survives sandbox / function timeouts gracefully.
//
// Three steps:
//   1. runCodeTurnStep        — invoke claude/opencode for one task body
//   2. persistCodeProjectStep — write manifest + outputs into the tenant's VFS
//   3. snapshotCodeAuthStep   — pull ~/.claude config out of the sandbox into
//                                Redis so a cold sandbox can resume the
//                                tenant's session history. Best-effort.

import { getStore } from "@/app/lib/store";
import { recordAudit, type AuditKind } from "@/app/lib/auditLog";
import {
  runClaudeCode,
  awaitClaudeCommand,
  snapshotClaudeAuth,
  restoreClaudeAuth,
} from "@/app/lib/sandboxClaudeCode";
import {
  appendCodeTask,
  appendCodeThought,
  getCodeProject,
  getCodeTasks,
  updateCodeProject,
  type CodeEngine,
  type CodeProjectMeta,
  type CodeProjectThought,
} from "@/app/lib/codeProjectStore";

// --- VFS (mirrors the schema used by persistDeepJobStep + agentTurn) -------

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
function vfsPathsKey(uid: string, sid: string) {
  return `vfs:${uid}:${sid}:paths`;
}
function vfsNodeKey(uid: string, sid: string, p: string) {
  return `vfs:${uid}:${sid}:node:${sanitizePath(p)}`;
}
type VfsFileNode = {
  type: "file";
  path: string;
  content: string;
  createdAt: string;
  updatedAt: string;
};
async function vfsWriteFile(args: {
  userId: string;
  sessionId: string;
  path: string;
  content: string;
}) {
  const store = getStore();
  const safe = sanitizePath(args.path);
  const now = new Date().toISOString();
  const existing = await store.get<VfsFileNode>(
    vfsNodeKey(args.userId, args.sessionId, safe)
  );
  const node: VfsFileNode = {
    type: "file",
    path: safe,
    content: args.content,
    createdAt: existing?.createdAt ?? now,
    updatedAt: now,
  };
  await store.set(vfsNodeKey(args.userId, args.sessionId, safe), node);
  await store.sadd(vfsPathsKey(args.userId, args.sessionId), safe);
  return safe;
}

// --- Workflow-body Redis shims --------------------------------------------
//
// Workflow functions execute inside the WDK VM which intentionally hides
// global `fetch` (it would break determinism). Every read/write against
// Upstash's REST API uses fetch under the hood, so any Redis call from the
// workflow body has to go through a `"use step"` boundary. These four
// helpers are thin wrappers around the project-store CRUD that the
// workflow needs at the orchestration layer (the actual heavy steps —
// runCodeTurnStep et al — already touch Redis from inside their own
// `"use step"` bodies, so they're fine).

export async function loadCodeProjectStep(args: {
  projectId: string;
}): Promise<CodeProjectMeta | null> {
  "use step";
  return getCodeProject(args.projectId);
}

export async function appendCodeThoughtStep(args: {
  projectId: string;
  thought: Omit<CodeProjectThought, "ts"> & { ts?: number };
}): Promise<void> {
  "use step";
  await appendCodeThought(args.projectId, args.thought);
  // Mirror transitions to the per-tenant audit log so the /ui Activity
  // panel shows every phase of every /code turn, not just dispatch/done.
  // We look up the project's tenantId here rather than threading it
  // through every call site — small extra Redis read per breadcrumb.
  if (args.thought.kind === "transition") {
    try {
      const proj = await getCodeProject(args.projectId);
      if (proj?.tenantId) {
        await recordAudit(proj.tenantId, {
          kind: "tool.code_progress",
          summary: `${args.projectId} ${args.thought.text}`,
          meta: { projectId: args.projectId },
        });
      }
    } catch {
      // best-effort; never let audit failure abort the workflow
    }
  }
}

export async function updateCodeProjectStep(args: {
  projectId: string;
  patch: Partial<CodeProjectMeta>;
}): Promise<CodeProjectMeta | null> {
  "use step";
  return updateCodeProject(args.projectId, args.patch);
}

// Surface a /code lifecycle event in the per-tenant audit log so it shows
// up in the /ui Activity panel (which filters to tool.* kinds). Callable
// from inside a workflow body via the step boundary.
export async function recordCodeAuditStep(args: {
  tenantId: string;
  kind: AuditKind;
  summary: string;
  meta?: Record<string, unknown>;
}): Promise<void> {
  "use step";
  await recordAudit(args.tenantId, {
    kind: args.kind,
    summary: args.summary,
    meta: args.meta,
  });
}

// Log a per-turn transition breadcrumb AND mirror it to the per-tenant
// audit log in one shot. Called from inside other steps (where Redis
// access is fine) so the /ui Activity panel and the /code status log
// stay in lockstep. Best-effort on the audit side — never blocks the
// step on a logging hiccup.
async function logCodeTransition(
  projectId: string,
  tenantId: string,
  text: string,
  data?: Record<string, unknown>
): Promise<void> {
  await appendCodeThought(projectId, { kind: "transition", text, data });
  try {
    await recordAudit(tenantId, {
      kind: "tool.code_progress",
      summary: `${projectId} ${text}`,
      meta: { projectId, ...(data ?? {}) },
    });
  } catch {
    // best-effort
  }
}

// --- Auth snapshot (per-tenant ~/.claude) ----------------------------------

// Redis key for the per-tenant claude auth/session snapshot.
function authSnapKey(tid: string) {
  return `codeproj:auth:${tid}`;
}

export async function snapshotCodeAuthStep(args: {
  tenantId: string;
  // Optional — pass the project's sandboxWorkdir so the snapshot also picks
  // up the per-project `.claude/` and `.opencode/` session logs that live
  // inside the workdir (not in the tenant-wide config dir). Without this,
  // every /code attach turn starts with no session history.
  projectWorkdir?: string;
}) {
  "use step";
  try {
    const snap = await snapshotClaudeAuth(args.tenantId, args.projectWorkdir);
    if (snap && Object.keys(snap).length > 0) {
      await getStore().set(authSnapKey(args.tenantId), snap);
    }
    return { ok: true, files: Object.keys(snap).length };
  } catch (err: any) {
    return { ok: false, files: 0, error: err?.message ?? String(err) };
  }
}

export async function restoreCodeAuthStep(args: {
  tenantId: string;
  projectWorkdir?: string;
}) {
  "use step";
  try {
    const snap =
      (await getStore().get<Record<string, string>>(
        authSnapKey(args.tenantId)
      )) ?? {};
    if (Object.keys(snap).length === 0) return { ok: true, files: 0 };

    // Hard deadline on the restore. Each file in the snapshot triggers a
    // separate sandbox round-trip; on big snapshots that's potentially
    // many seconds. Restore is best-effort by design (the engine still
    // runs without prior session history), so if the budget blows we
    // proceed rather than stalling the whole codeWorkflow.
    const RESTORE_DEADLINE_MS = 30_000;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(
        () => reject(new Error(`restore exceeded ${RESTORE_DEADLINE_MS}ms`)),
        RESTORE_DEADLINE_MS
      );
    });
    try {
      await Promise.race([
        restoreClaudeAuth(args.tenantId, snap, args.projectWorkdir),
        deadline,
      ]);
    } finally {
      if (timer) clearTimeout(timer);
    }
    return { ok: true, files: Object.keys(snap).length };
  } catch (err: any) {
    return { ok: false, files: 0, error: err?.message ?? String(err) };
  }
}

// --- Main coding turn ------------------------------------------------------

export type RunCodeTurnInput = {
  projectId: string;
  task: string;
  // Bumped by the workflow on each turn so we can name output files.
  turn: number;
};

export type RunCodeTurnResult = {
  ok: boolean;
  output: string;
  error?: string;
  // Path under tenant VFS where this turn's output was written.
  vfsOutputPath?: string;
  // Engine that actually ran (matches project.engine).
  engine: CodeEngine;
  // Updated turn count in project meta.
  turnCount: number;
};

// Redis key for the cmdId of an in-flight engine command. Keyed by
// (projectId, turn) so step retries don't re-dispatch — they rehydrate
// the existing command. Cleared by finalizeCodeTurnStep on completion.
function turnCmdIdKey(projectId: string, turn: number) {
  return `codeproj:turn:cmdid:${projectId}:${turn}`;
}

export async function runCodeTurnStep(
  args: RunCodeTurnInput
): Promise<RunCodeTurnResult & { cmdId?: string }> {
  "use step";

  const proj = await getCodeProject(args.projectId);
  if (!proj) {
    return {
      ok: false,
      output: "",
      error: `unknown project ${args.projectId}`,
      engine: "claude",
      turnCount: 0,
    };
  }

  await updateCodeProject(args.projectId, {
    status: "working",
    currentTask: args.task,
    lastError: undefined,
  });
  await logCodeTransition(
    args.projectId,
    proj.tenantId,
    `→ working (turn ${args.turn})`,
    { engine: proj.engine, task: args.task.slice(0, 200) }
  );

  // Idempotency: if a prior invocation of this step already dispatched
  // the engine (e.g. the workflow ticked, the function instance died
  // before we could persist the cmdId-returning success, and WDK is
  // retrying us), just reuse the in-flight command. We persist cmdId
  // before any error path so a retried dispatch never accidentally
  // launches a second engine run on the same workdir.
  const store = getStore();
  const existingCmdId = await store.get<string>(
    turnCmdIdKey(args.projectId, args.turn)
  );
  if (existingCmdId) {
    return {
      ok: true,
      output: "",
      engine: proj.engine,
      turnCount: args.turn,
      cmdId: existingCmdId,
    };
  }

  // continueSession on every turn after the first so the engine picks up
  // its own session log instead of starting fresh.
  const isFirstTurn = args.turn <= 1;

  const dispatch = await runClaudeCode({
    prompt: args.task,
    tenantId: proj.tenantId,
    continueSession: !isFirstTurn,
    absoluteWorkdir: proj.sandboxWorkdir,
    forceEngine: proj.engine,
    repoUrl: proj.repoUrl,
    baseBranch: proj.baseBranch,
    // Detached mode is what makes long-running /code turns survive the
    // Vercel function timeout: the engine runs INSIDE the sandbox; the
    // workflow loops awaitCodeTurnStep on subsequent function invocations
    // until the command finishes.
    detached: true,
    // Per-phase progress callback. runClaudeCode invokes this at three
    // major checkpoints — sandbox bootstrap, repo clone, and engine
    // start — so `/code status` and the /ui Activity panel both show the
    // workflow ticking through them instead of going silent for minutes
    // between "→ working" and "→ result".
    onProgress: async (phase) => {
      try {
        await logCodeTransition(args.projectId, proj.tenantId, `→ ${phase}`);
      } catch {
        // best-effort; progress reporting failures should not abort the turn
      }
    },
  });

  // Dispatch failures (bad GitHub token, sandbox unavailable, etc.) are
  // synchronous and have no cmdId to wait on. Surface them and fail
  // immediately.
  if (!dispatch.ok || !dispatch.error?.startsWith("__cmdId:")) {
    const turnCount = args.turn;
    const err = dispatch.error ?? "dispatch failed";
    await appendCodeThought(args.projectId, {
      kind: "error",
      text: `engine dispatch failed: ${err}`,
    });
    await appendCodeTask(args.projectId, {
      turn: turnCount,
      ts: Date.now(),
      task: args.task,
      status: "failed",
      error: err,
    });
    await updateCodeProject(args.projectId, {
      status: "failed",
      lastError: err,
      turnCount,
    });
    return {
      ok: false,
      output: "",
      error: err,
      engine: proj.engine,
      turnCount,
    };
  }

  const cmdId = dispatch.error.slice("__cmdId:".length);
  await store.set(turnCmdIdKey(args.projectId, args.turn), cmdId, {
    exSeconds: 24 * 60 * 60,
  });

  return {
    ok: true,
    output: "",
    engine: proj.engine,
    turnCount: args.turn,
    cmdId,
  };
}

// Poll a detached engine command. Returns { done: false } if the command
// is still running after the per-call deadline — the workflow loops and
// calls us again on a fresh function invocation. Returns the full result
// once the engine finishes (or errors out). The codeWorkflow then calls
// finalizeCodeTurnStep with the result to do VFS writes + status updates.
export type AwaitCodeTurnResult =
  | { done: false }
  | {
      done: true;
      ok: boolean;
      output: string;
      exitCode: number;
      error?: string;
    };

export async function awaitCodeTurnStep(args: {
  projectId: string;
  cmdId: string;
}): Promise<AwaitCodeTurnResult> {
  "use step";

  // Each poll is a fast shell round-trip (one `test -f` + maybe `cat`),
  // so without an inter-poll sleep we'd burn the workflow's MAX_POLLS
  // cap in seconds and not actually give the engine time to finish.
  // Poll every few seconds for up to ~200s (well under any reasonable
  // function timeout), then yield to the workflow so it can checkpoint
  // and let this function instance retire. The next iteration of the
  // workflow loop picks up on a fresh instance and re-polls.
  const POLL_INTERVAL_MS = 5_000;
  const STEP_BUDGET_MS = 200_000;
  const start = Date.now();
  while (Date.now() - start < STEP_BUDGET_MS) {
    const probe = await awaitClaudeCommand({ cmdId: args.cmdId });
    if (probe.done) return probe;
    await new Promise<void>((r) => setTimeout(r, POLL_INTERVAL_MS));
  }
  return { done: false };
}

// Final pass after the detached engine completes. Writes per-turn
// output/task files to the tenant VFS, updates project status, and
// appends the result thought. Cleared the in-flight cmdId so a re-run
// of the same turn would re-dispatch cleanly.
export async function finalizeCodeTurnStep(args: {
  projectId: string;
  turn: number;
  task: string;
  engineResult: {
    ok: boolean;
    output: string;
    exitCode: number;
    error?: string;
  };
}): Promise<RunCodeTurnResult> {
  "use step";

  const proj = await getCodeProject(args.projectId);
  if (!proj) {
    return {
      ok: false,
      output: "",
      error: `unknown project ${args.projectId}`,
      engine: "claude",
      turnCount: args.turn,
    };
  }

  // Drop the in-flight cmdId key — we're done with it.
  await getStore().del(turnCmdIdKey(args.projectId, args.turn));

  const turnCount = args.turn;
  const result = args.engineResult;

  if (!result.ok) {
    await appendCodeThought(args.projectId, {
      kind: "error",
      text: `engine error: ${result.error ?? "unknown"}`,
      data: { exitCode: result.exitCode },
    });
    await appendCodeTask(args.projectId, {
      turn: turnCount,
      ts: Date.now(),
      task: args.task,
      status: "failed",
      error: result.error ?? "engine failed",
    });
    await updateCodeProject(args.projectId, {
      status: "failed",
      lastError: result.error ?? "engine failed",
      turnCount,
    });
    return {
      ok: false,
      output: result.output ?? "",
      error: result.error,
      engine: proj.engine,
      turnCount,
    };
  }

  // Write per-turn artifacts to VFS under the project's vfsRoot.
  let vfsOutputPath: string | undefined;
  try {
    vfsOutputPath = await vfsWriteFile({
      userId: proj.tenantId,
      sessionId: proj.sessionId,
      path: `${proj.vfsRoot}/output-${turnCount}.md`,
      content:
        `# ${proj.title} — turn ${turnCount}\n\n` +
        `**Task:** ${args.task}\n\n` +
        `---\n\n` +
        result.output,
    });
    await vfsWriteFile({
      userId: proj.tenantId,
      sessionId: proj.sessionId,
      path: `${proj.vfsRoot}/task-${turnCount}.md`,
      content: args.task,
    });
  } catch (err: any) {
    await appendCodeThought(args.projectId, {
      kind: "error",
      text: `vfs write failed (non-fatal): ${err?.message ?? String(err)}`,
    });
  }

  await appendCodeTask(args.projectId, {
    turn: turnCount,
    ts: Date.now(),
    task: args.task,
    status: "done",
    outputPreview: result.output.slice(0, 600),
  });
  await appendCodeThought(args.projectId, {
    kind: "result",
    text:
      result.output.length > 280
        ? result.output.slice(0, 280) + "…"
        : result.output || "(no output)",
  });

  await updateCodeProject(args.projectId, {
    status: "awaiting_followup",
    lastOutput: result.output.slice(0, 4000),
    turnCount,
  });

  return {
    ok: true,
    output: result.output,
    engine: proj.engine,
    turnCount,
    vfsOutputPath,
  };
}

// --- Manifest write --------------------------------------------------------
//
// Materializes a manifest summarizing the project to VFS. Called at the end
// of each turn so /workspace/claude_code/projects/{id}/manifest.json is
// always current.

export type PersistCodeProjectInput = { projectId: string };
export type PersistCodeProjectResult = {
  ok: boolean;
  manifestPath?: string;
  error?: string;
};

export async function persistCodeProjectStep(
  args: PersistCodeProjectInput
): Promise<PersistCodeProjectResult> {
  "use step";
  const proj = await getCodeProject(args.projectId);
  if (!proj) return { ok: false, error: `unknown project ${args.projectId}` };

  try {
    const tasks = await getCodeTasks(args.projectId, 100);
    const manifest = {
      projectId: proj.projectId,
      title: proj.title,
      engine: proj.engine,
      status: proj.status,
      createdAt: new Date(proj.createdAt).toISOString(),
      updatedAt: new Date(proj.updatedAt).toISOString(),
      turnCount: proj.turnCount,
      sandboxWorkdir: proj.sandboxWorkdir,
      repoUrl: proj.repoUrl,
      baseBranch: proj.baseBranch,
      pushedBranch: proj.pushedBranch,
      tasks: tasks.map((t) => ({
        turn: t.turn,
        task: t.task,
        status: t.status,
        ts: new Date(t.ts).toISOString(),
        outputPreview: t.outputPreview,
        error: t.error,
      })),
    };
    const manifestPath = await vfsWriteFile({
      userId: proj.tenantId,
      sessionId: proj.sessionId,
      path: `${proj.vfsRoot}/manifest.json`,
      content: JSON.stringify(manifest, null, 2),
    });
    return { ok: true, manifestPath };
  } catch (err: any) {
    return { ok: false, error: err?.message ?? String(err) };
  }
}
