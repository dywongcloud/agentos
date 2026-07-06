// app/workflows/codeWorkflow.ts
//
// Long-running coding-project runner. One workflow invocation = one turn
// against an existing CodeProject. Designed so the user can dispatch and
// move on; status is queryable via /code status PROJECT_ID.
//
// The workflow pipeline is linear:
//   1. restore tenant's claude config from Redis snapshot (best-effort)
//   2. run a single coding turn (claude or opencode in the shared sandbox)
//   3. persist a manifest to VFS so the project survives a sandbox cold
//      start
//   4. snapshot the claude config dir back to Redis so the NEXT turn (in
//      possibly a fresh sandbox) inherits session history
//   5. send a completion message to the tenant's chat
//
// XState would be overkill here — there's no branching, just sequential
// steps. If the function instance is torn down between steps, WDK resumes
// from the durable log.
//
// IMPORTANT: every Redis read/write goes through a `"use step"`. The WDK
// VM strips global `fetch` from workflow bodies, and Upstash's REST
// client uses fetch under the hood — calling getCodeProject /
// appendCodeThought / updateCodeProject directly here will fail with
// "Global fetch is unavailable in workflow functions". The thin step
// wrappers live in codeSteps.ts (loadCodeProjectStep,
// appendCodeThoughtStep, updateCodeProjectStep).

import {
  appendCodeThoughtStep,
  awaitCodeTurnStep,
  finalizeCodeTurnStep,
  loadCodeProjectStep,
  persistCodeProjectStep,
  recordCodeAuditStep,
  restoreCodeAuthStep,
  runCodeTurnStep,
  snapshotCodeAuthStep,
  updateCodeProjectStep,
} from "@/app/steps/codeSteps";
import { sendOutbound } from "@/app/steps/sendOutbound";
import { recordSolutionStep } from "@/app/steps/solutionSteps";

export async function codeWorkflow(projectId: string, taskBody: string) {
  "use workflow";

  const proj = await loadCodeProjectStep({ projectId });
  if (!proj) {
    // Project was deleted between dispatch and pick-up — nothing to do.
    return;
  }

  try {
    // Progress breadcrumb so `/code status` shows the workflow ticking
    // forward even while we're inside the restore step (which can do many
    // sandbox round-trips on tenants with a big prior snapshot).
    await appendCodeThoughtStep({
      projectId,
      thought: { kind: "transition", text: "→ workflow started" },
    });

    const nextTurn = (proj.turnCount ?? 0) + 1;

    // 1. Restore claude/opencode config snapshot. Skipped on turn 1: there's
    // no session history to restore (the project was just created), and
    // opencode re-adds its auth on every invocation inside runClaudeCode,
    // so first-turn restore is pure overhead. Skipping shaves ~5-30s off
    // the cold-start path on tenants whose snapshot grew from earlier
    // /code projects.
    if (nextTurn > 1) {
      await appendCodeThoughtStep({
        projectId,
        thought: { kind: "transition", text: "→ restoring session snapshot" },
      });
      await restoreCodeAuthStep({
        tenantId: proj.tenantId,
        projectWorkdir: proj.sandboxWorkdir,
      });
    }

    // 2. Dispatch the engine in detached mode. Returns a cmdId we can
    // poll across many function-instance lifetimes — the engine runs
    // inside the sandbox, NOT inside this function call.
    const dispatch = await runCodeTurnStep({
      projectId,
      task: taskBody,
      turn: nextTurn,
    });

    // 2b. Wait for the engine to finish. The poll step has a per-call
    // deadline well under any Vercel function timeout; if the deadline
    // fires the workflow loops onto a fresh function instance and the
    // SAME long-running command is rehydrated and waited on again. Cap
    // total poll cycles so a permanently-stuck engine can't loop
    // forever (50 polls × 3.5 min ≈ ~3 hours, more than enough for any
    // real /code task; sandbox 1h timeout is the actual ceiling).
    let turn: import("@/app/steps/codeSteps").RunCodeTurnResult = dispatch;
    if (dispatch.ok && dispatch.cmdId) {
      let polls = 0;
      const MAX_POLLS = 50;
      let engineResult:
        | { done: true; ok: boolean; output: string; exitCode: number; error?: string }
        | null = null;
      while (polls++ < MAX_POLLS) {
        const probe = await awaitCodeTurnStep({
          projectId,
          cmdId: dispatch.cmdId,
        });
        if (probe.done) {
          engineResult = probe;
          break;
        }
      }
      if (!engineResult) {
        engineResult = {
          done: true,
          ok: false,
          output: "",
          exitCode: -1,
          error: `engine still running after ${MAX_POLLS} poll cycles`,
        };
      }
      turn = await finalizeCodeTurnStep({
        projectId,
        turn: nextTurn,
        task: taskBody,
        engineResult: {
          ok: engineResult.ok,
          output: engineResult.output,
          exitCode: engineResult.exitCode,
          error: engineResult.error,
        },
      });
    }

    // 3-5. Three independent side-effect steps run concurrently:
    //   - persistCodeProjectStep: refresh the manifest in VFS so
    //     `/code status` reads a consistent view.
    //   - snapshotCodeAuthStep: snapshot per-project session logs back
    //     to Redis so the next `/code attach` resumes the session.
    //   - recordCodeAuditStep: emit the audit-log entry for /ui's
    //     Activity panel.
    // None of these feed each other's inputs and all of them must
    // complete before we deliver the outcome message. Running them as
    // a Promise.all (a) shaves a couple seconds off long turns, and
    // (b) renders as a parallel fan-out in the workflow graph.
    //
    // engineLabel is derived from `proj.engine` (already loaded at the
    // top of the workflow) instead of refetching via loadCodeProjectStep
    // — engine selection is fixed at project creation and never changes
    // during a turn, so the previous defensive `refreshed?.engine`
    // lookup was never load-bearing.
    const engineLabel = proj.engine === "opencode" ? "OpenCode" : "Claude Code";
    await Promise.all([
      persistCodeProjectStep({ projectId }),
      snapshotCodeAuthStep({
        tenantId: proj.tenantId,
        projectWorkdir: proj.sandboxWorkdir,
      }),
      recordCodeAuditStep({
        tenantId: proj.tenantId,
        kind: turn.ok ? "tool.code_turn_done" : "tool.code_turn_failed",
        summary: turn.ok
          ? `${engineLabel} turn ${turn.turnCount} done on ${projectId}`
          : `${engineLabel} turn ${turn.turnCount} failed on ${projectId}: ${(turn.error ?? "unknown").slice(0, 120)}`,
        meta: {
          projectId,
          engine: turn.engine,
          turn: turn.turnCount,
          vfsOutputPath: turn.vfsOutputPath,
          error: turn.error,
        },
      }),
    ]);
    if (turn.ok) {
      const out = (turn.output || "(no output)").trim();
      // Procedural memory: remember how this coding task was solved so a future
      // similar task reuses the approach. Best-effort, one embedding.
      if (proj.title?.trim() && out && out !== "(no output)") {
        await recordSolutionStep({
          tenantId: proj.tenantId,
          meta: {
            source: "code",
            task: proj.title,
            outcome: out,
            toolsUsed: [engineLabel],
            tags: ["code"],
            ref: projectId,
          },
        });
      }
      const preview = out.length > 3200 ? out.slice(0, 3200) + "…" : out;
      await sendOutbound({
        channel: proj.channel,
        sessionId: proj.sessionId,
        text:
          `${engineLabel.toLowerCase()} done with turn ${turn.turnCount} on ${projectId}:\n\n` +
          preview +
          `\n\n— /code attach ${projectId} <next> to keep going, /code push ${projectId} to ship it.`,
      });
    } else {
      await sendOutbound({
        channel: proj.channel,
        sessionId: proj.sessionId,
        text:
          `that ${engineLabel.toLowerCase()} turn (${projectId} #${turn.turnCount}) bailed out:\n\n` +
          (turn.error ?? "unknown error").slice(0, 1200) +
          `\n\n/code attach ${projectId} <retry> or /code status ${projectId} to dig in.`,
      });
    }
  } catch (err: any) {
    const msg = err?.message ?? String(err);
    await appendCodeThoughtStep({
      projectId,
      thought: { kind: "error", text: `workflow exception: ${msg}` },
    });
    await updateCodeProjectStep({
      projectId,
      patch: { status: "failed", lastError: msg },
    });
    await recordCodeAuditStep({
      tenantId: proj.tenantId,
      kind: "tool.code_turn_failed",
      summary: `${projectId} workflow crashed: ${msg.slice(0, 120)}`,
      meta: { projectId, error: msg },
    });
    try {
      await sendOutbound({
        channel: proj.channel,
        sessionId: proj.sessionId,
        text: `/code ${projectId} crashed on me: ${msg.slice(0, 500)}`,
      });
    } catch {
      // outbound failure isn't actionable here
    }
  }
}
