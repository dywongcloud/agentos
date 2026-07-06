// app/workflows/jobWorkflow.ts
//
// Durable runner for one job. The state graph lives in
// app/machines/jobMachine.ts; this file is the loop that drives it.
//
// Key invariant: every async unit of work the actor needs is a `"use step"`
// in app/steps/jobSteps.ts. The runner reads the current state, awaits the
// relevant step, sends an event to the actor, and persists the new snapshot
// to Redis. If the function instance is torn down between iterations, WDK
// resumes the workflow from its durable log and we rehydrate the actor from
// the persisted snapshot — so the machine state and the WDK step log stay
// in sync.

import { createActor } from "xstate";

import { jobMachine, type SubtaskResult } from "@/app/machines/jobMachine";

import {
  logTransitionStep,
  clarifyStep,
  planStep,
  executeAgentTurnStep,
  verifyStep,
  finalizeJobStep,
  failJobStep,
  isJobCancelledStep,
  loadJobMetaStep,
  loadJobSnapshotStep,
  saveJobSnapshotStep,
  markJobStartedStep,
  getJobCostStep,
  isJobEscalatedStep,
  getJobDepthStateStep,
  setJobEscalatedStep,
  bumpJobDepthPassStep,
} from "@/app/steps/jobSteps";
import { isWebSearchEnabled, webSearchStep } from "@/app/steps/webSearchStep";
import {
  orchestrateStep,
  MAX_ORCHESTRATOR_ITERATIONS,
  DEEP_PER_ATTEMPT_ITERS,
  type ParallelSubtaskSpec,
} from "@/app/steps/orchestrateStep";
import { reviewDepthStep } from "@/app/steps/reviewDepthStep";
import { resolveModelName, purposeForModality } from "@/app/lib/modelRouting";
import { deepBudgetUsd, deepEscalatedBudgetUsd } from "@/app/lib/costTracker";
import { persistDeepJobStep } from "@/app/steps/persistDeepJobStep";
import { codeInterpreterStep } from "@/app/steps/codeInterpreterStep";
import { compactSubtasksStep } from "@/app/steps/subtaskCompactionStep";

const MAX_TICKS = 32; // hard cap — prevents an unforeseen machine loop spinning forever

export async function jobWorkflow(jobId: string) {
  "use workflow";

  const meta = await loadJobMetaStep(jobId);
  if (!meta) return;

  // mark-started writes status/startedAt; snapshot-load reads a prior
  // XState snapshot. They touch disjoint Redis fields so they're safe
  // to run concurrently — neither's output feeds the other.
  const [, prior] = await Promise.all([
    markJobStartedStep({ jobId }),
    loadJobSnapshotStep(jobId),
  ]);

  const input = {
    jobId: meta.jobId,
    tenantId: meta.tenantId,
    sessionId: meta.sessionId,
    channel: meta.channel,
    prompt: meta.prompt,
    // Job kind set by the dispatcher's depth classifier ("research" → deep,
    // anything else → normal). Drives whether the machine takes the
    // orchestrating branch or the planning branch after clarify.
    kind: (meta.kind === "research" ? "deep" : "normal") as
      | "normal"
      | "deep",
  };

  const actor = prior
    ? createActor(jobMachine, { input, snapshot: prior as never })
    : createActor(jobMachine, { input });

  actor.start();

  // No hardcoded pre-flight connection gate. Connection health is enforced at
  // runtime by the executor's own tools (check_integration_connected + real
  // Composio errors), not by guessing the needed toolkit from the prompt.

  let delivered = false;
  let ticks = 0;

  try {
    while (ticks++ < MAX_TICKS) {
      const snap = actor.getSnapshot();
      const value = typeof snap.value === "string" ? snap.value : "";
      const ctx = snap.context;

      if (snap.status === "done") break;
      if (snap.status === "stopped") break;

      // Cooperative cancellation: /stop (and /start's reboot) mark active jobs
      // cancelled. Halt between steps so a long deep run stops promptly and does
      // NOT deliver a result. Meta is already "cancelled" (removed from active).
      if (await isJobCancelledStep(jobId)) return;

      if (value === "pending") {
        // Auto-transition handled by machine; just persist + loop.
        await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
        continue;
      }

      if (value === "clarifying") {
        await logTransitionStep({
          jobId,
          status: "clarifying",
          text: "→ clarifying",
        });
        // Clarifier never asks — it only records assumptions. The `needs_input`
        // machine state remains in the graph for future explicit-clarification
        // flows but is unreachable from this code path.
        const out = await clarifyStep({ jobId, prompt: ctx.prompt });
        actor.send({ type: "CLARIFY_DONE", assumptions: out.assumptions });
        await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
        continue;
      }

      if (value === "needs_input") {
        // Pause here. A future RESUME event arrives via a separate dispatch
        // call (?op=resume), which re-enters this workflow.
        await logTransitionStep({
          jobId,
          status: "needs_input",
          text: "awaiting user input",
          data: { question: ctx.pendingQuestion },
        });
        await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
        return;
      }

      if (value === "planning") {
        await logTransitionStep({
          jobId,
          status: "planning",
          text: "→ planning",
        });
        const out = await planStep({
          jobId,
          prompt: ctx.prompt,
          assumptions: ctx.assumptions,
          verifierNotes: ctx.verifierNotes,
        });
        actor.send({
          type: "PLAN_DONE",
          plan: out.plan,
          modality: out.modality,
        });
        await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
        continue;
      }

      if (value === "orchestrating") {
        await logTransitionStep({
          jobId,
          status: "executing", // surface as executing in meta — no separate UI state
          text: "→ orchestrating (deep mode)",
        });

        // Run orchestrator iterations until it says done, hits the iteration
        // cap, or burns the dollar budget. Escalated jobs (depth reviewer
        // bumped them to the pro tier) get the larger escalated budget.
        const escalatedNow = await isJobEscalatedStep(jobId);
        const budgetCap = escalatedNow ? deepEscalatedBudgetUsd() : deepBudgetUsd();

        // Run one subtask spec (research / compute / execute / synthesize)
        // through the right step and shape the result. Factored out so the
        // sequential path and the parallel fan-out share identical dispatch.
        const runSubtask = async (
          spec: ParallelSubtaskSpec,
          subIter: number
        ): Promise<SubtaskResult> => {
          const subtaskId = `s_${subIter + 1}_${Date.now().toString(36)}_${Math.random()
            .toString(36)
            .slice(2, 6)}`;
          if (spec.action === "research") {
            const sr = await webSearchStep({ jobId, query: spec.query ?? spec.goal });
            return {
              id: subtaskId,
              iter: subIter,
              kind: "research",
              goal: spec.goal,
              output: sr.findings,
              artifacts: [],
              citations: sr.citations,
              ts: Date.now(),
            };
          }
          if (spec.action === "compute") {
            const cr = await codeInterpreterStep({
              jobId,
              goal: spec.goal,
              instructions: spec.instructions || spec.goal,
            });
            return {
              id: subtaskId,
              iter: subIter,
              kind: "execute",
              goal: spec.goal,
              output: cr.ok ? cr.text : `code_interpreter failed: ${cr.error ?? "unknown"}`,
              artifacts: cr.fileRefs,
              citations: [],
              ts: Date.now(),
            };
          }
          // execute / synthesize → agentTurn. Same model-selection logic as the
          // sequential path: synthesize is cheap text-combination, code modality
          // uses the coding model, everything else uses the smart workhorse.
          const subtaskPurpose =
            spec.action === "synthesize"
              ? escalatedNow
                ? "smart-pro"
                : "fast-meta"
              : spec.modality && spec.modality.startsWith("code-")
                ? "coding"
                : "smart";
          const er = await executeAgentTurnStep({
            jobId,
            tenantId: meta.tenantId,
            sessionId: meta.sessionId,
            channel: meta.channel,
            prompt: spec.instructions || spec.goal,
            showTyping: false,
            modelName: resolveModelName(subtaskPurpose),
          });
          return {
            id: subtaskId,
            iter: subIter,
            kind: spec.action,
            goal: spec.goal,
            output: er.text,
            artifacts: er.artifacts,
            citations: [],
            ts: Date.now(),
          };
        };

        let iter = ctx.subtaskResults.length;
        let finalized = false;
        // Per-attempt iteration budget: how many orchestrator steps this
        // single orchestrating entry may take before it MUST draft a final
        // answer. This is what lets the verifier + depth reviewer actually
        // run — without it the orchestrator can research indefinitely on one
        // attempt and never hand a draft to the reviewers. Each review cycle
        // (more_passes / escalate) starts a fresh attempt with a new budget.
        const perAttemptCap = DEEP_PER_ATTEMPT_ITERS;
        const attemptStart = iter;
        while (iter < MAX_ORCHESTRATOR_ITERATIONS) {
          const snapNow = actor.getSnapshot();
          const ctxNow = snapNow.context;
          const forceFinalize = iter - attemptStart >= perAttemptCap;

          // Cost gate: check accumulated spend BEFORE the orchestrator call so
          // we never pay for an extra reasoning round past the cap.
          const cost = await getJobCostStep(jobId);
          if (cost.usd >= budgetCap) {
            const last = ctxNow.subtaskResults.slice(-1)[0];
            await logTransitionStep({
              jobId,
              status: "executing",
              text: `cost cap reached ($${cost.usd.toFixed(3)} ≥ $${budgetCap.toFixed(2)}) — finalizing with last subtask`,
              data: { costUsd: cost.usd, capUsd: budgetCap },
            });
            actor.send({
              type: "ORCHESTRATE_FINAL",
              finalText:
                last?.output ??
                "Cost cap reached before producing a complete answer. Increase BUDGET_USD_PER_DEEP_JOB to allow longer runs.",
              artifacts: last?.artifacts ?? [],
              modality: "generic",
            });
            await saveJobSnapshotStep({
              jobId,
              snapshot: actor.getPersistedSnapshot(),
            });
            finalized = true;
            break;
          }

          // Soft compaction: when subtaskResults gets large, summarize older
          // entries so the orchestrator's prompt stays bounded. Compaction
          // step is a no-op below the keep-recent threshold.
          const compactedSubtasks = await compactSubtasksStep({
            jobId,
            subtaskResults: ctxNow.subtaskResults,
          });

          const decision = await orchestrateStep({
            jobId,
            prompt: ctxNow.prompt,
            assumptions: ctxNow.assumptions,
            subtaskResults: compactedSubtasks,
            verifierNotes: ctxNow.verifierNotes,
            iter,
            costUsd: cost.usd,
            escalated: escalatedNow,
            forceFinalize,
          });

          if (decision.action === "done") {
            actor.send({
              type: "ORCHESTRATE_FINAL",
              finalText: decision.finalSynthesis ?? "",
              artifacts: [],
              modality: decision.modality ?? "generic",
            });
            await saveJobSnapshotStep({
              jobId,
              snapshot: actor.getPersistedSnapshot(),
            });
            finalized = true;
            break;
          }

          // Parallel fan-out: the orchestrator declared a batch of independent
          // subtasks. Run them all at once as nested sub-agents (each a full
          // step — webSearch / codeInterpreter / agentTurn), then fold every
          // result into the machine and advance the iteration counter by the
          // batch size so per-attempt + iteration caps still hold. The single
          // dollar-budget gate above already cleared this turn.
          const batch = decision.parallelSubtasks;
          if (batch && batch.length > 0) {
            const specs = batch.slice(0, 4);
            await logTransitionStep({
              jobId,
              status: "executing",
              text: `parallel fan-out: ${specs.length} sub-agents (${specs
                .map((s) => s.action)
                .join(", ")})`,
              data: { count: specs.length },
            });
            const results = await Promise.all(
              specs.map((spec, i) => runSubtask(spec, iter + i))
            );
            for (const r of results) actor.send({ type: "SUBTASK_DONE", result: r });
            await saveJobSnapshotStep({
              jobId,
              snapshot: actor.getPersistedSnapshot(),
            });
            iter += results.length;
            continue;
          }

          // Sequential single-subtask path. `decision.action` is one of
          // research/execute/synthesize/compute here ("done" already broke out).
          const result = await runSubtask(
            {
              action: decision.action as ParallelSubtaskSpec["action"],
              goal: decision.goal,
              instructions: decision.instructions,
              query: decision.query,
              modality: decision.modality,
            },
            iter
          );

          actor.send({ type: "SUBTASK_DONE", result });
          await saveJobSnapshotStep({
            jobId,
            snapshot: actor.getPersistedSnapshot(),
          });
          iter++;
        }

        // Hit cap without orchestrator saying done → force-finalize with the
        // last subtask's output.
        if (!finalized) {
          const last = actor.getSnapshot().context.subtaskResults.slice(-1)[0];
          actor.send({
            type: "ORCHESTRATE_FINAL",
            finalText:
              last?.output ??
              "Reached orchestrator iteration cap without producing an answer.",
            artifacts: last?.artifacts ?? [],
            modality: "generic",
          });
          await saveJobSnapshotStep({
            jobId,
            snapshot: actor.getPersistedSnapshot(),
          });
        }
        continue;
      }

      if (value === "executing") {
        await logTransitionStep({
          jobId,
          status: "executing",
          text: "→ executing",
        });

        // Research subagent pre-pass: only for research-modality jobs and
        // only when env OPENAI_WEB_SEARCH_ENABLED=true. Failure is non-fatal —
        // webSearchStep already logs + returns empty on error.
        let augmentedPrompt = ctx.prompt;
        if (ctx.modality === "research" && isWebSearchEnabled()) {
          const findings = await webSearchStep({ jobId, query: ctx.prompt });
          if (findings.findings) {
            augmentedPrompt = [
              ctx.prompt,
              "",
              "--- Researcher subagent notes (web search) ---",
              findings.findings,
              findings.citations.length
                ? `\nSources:\n- ${findings.citations.join("\n- ")}`
                : "",
              "--- end notes ---",
              "",
              "Use the notes above to write the final answer. Cite sources",
              "inline by URL where appropriate.",
            ]
              .filter(Boolean)
              .join("\n");
          }
        }

        // Pick the right model based on the planner's modality classification.
        // code-* modalities route to the coding model (gpt-5.3-codex by default);
        // everything else uses the smart workhorse.
        const executorPurpose = purposeForModality(ctx.modality);
        const executorModel = resolveModelName(executorPurpose);

        const out = await executeAgentTurnStep({
          jobId,
          tenantId: ctx.tenantId,
          sessionId: ctx.sessionId,
          channel: ctx.channel,
          prompt: augmentedPrompt,
          showTyping: ctx.channel === "telegram",
          modelName: executorModel,
        });
        delivered = delivered || out.delivered;
        actor.send({
          type: "EXECUTE_DONE",
          text: out.text,
          artifacts: out.artifacts,
        });
        await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
        continue;
      }

      if (value === "verifying") {
        await logTransitionStep({
          jobId,
          status: "verifying",
          text: "→ verifying",
        });
        const resultText = ctx.executionResult?.text ?? "";
        const out = await verifyStep({
          jobId,
          prompt: ctx.prompt,
          resultText,
          modality: ctx.modality ?? "generic",
        });

        if (!out.pass) {
          // Correctness failed — revise as before.
          actor.send({ type: "VERIFY_REVISE", notes: out.notes });
          await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
          continue;
        }

        // Confidence signal: the critic passed but flagged LOW confidence — a
        // weak "okay I guess" rather than a clean pass. On the FIRST pass only
        // (reviseCount === 0) spend exactly one extra revise to firm it up,
        // before any depth review. Bounded by reviseCount so it can't spin: the
        // re-verified result ships regardless of the second confidence reading,
        // and if revise budget is gone the machine just accepts (→ done), so
        // this never turns a passing job into a failure. Aligns with the
        // depth-over-cost preference for deep work.
        if (out.confidence === "low" && (ctx.reviseCount ?? 0) === 0) {
          await logTransitionStep({
            jobId,
            status: "executing",
            text: "verify passed but LOW confidence — one extra pass to firm it up",
            data: { confidence: out.confidence },
          });
          actor.send({
            type: "VERIFY_REVISE",
            notes:
              out.notes.length > 0
                ? out.notes
                : [
                    "The result passed the correctness check but the critic was " +
                      "only LOW confidence. Strengthen the weakest parts: add " +
                      "concrete evidence/specifics, remove hand-waving, and make " +
                      "sure every part of the request is fully addressed.",
                  ],
          });
          await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
          continue;
        }

        // Correctness passed. For DEEP jobs, run the depth reviewer: is this
        // genuinely insightful + data-rich, or can we dig deeper? It can push
        // for more passes and/or escalate the model tier. Governed by a depth
        // -pass cap, the dollar budget, and the machine's MAX_REVISE_PASSES.
        const depthState = await getJobDepthStateStep(jobId);
        const cost = await getJobCostStep(jobId);
        const cap = depthState.escalated ? deepEscalatedBudgetUsd() : deepBudgetUsd();

        if (
          ctx.kind === "deep" &&
          depthState.depthPasses < depthState.maxDepthPasses &&
          cost.usd < cap * 0.9
        ) {
          const review = await reviewDepthStep({
            jobId,
            prompt: ctx.prompt,
            draft: resultText,
            subtaskResults: ctx.subtaskResults,
            alreadyEscalated: depthState.escalated,
            depthPass: depthState.depthPasses + 1,
            costUsd: cost.usd,
            budgetCap: cap,
          });

          if (review.verdict === "accept") {
            actor.send({ type: "VERIFY_PASS" });
          } else {
            // more_passes or escalate → loop back to orchestrating with the
            // depth gaps as notes. Escalate flips the pro-tier flag first.
            if (review.verdict === "escalate") {
              await setJobEscalatedStep({ jobId, escalated: true });
              await logTransitionStep({
                jobId,
                status: "executing",
                text: `depth reviewer ESCALATED to pro tier (avg=${review.avg.toFixed(1)}) — re-attacking gaps on gpt-5.4-pro + gemini cross-review`,
                data: { scores: review.scores },
              });
            }
            await bumpJobDepthPassStep({ jobId });
            actor.send({ type: "VERIFY_REVISE", notes: review.gaps });
          }
        } else {
          actor.send({ type: "VERIFY_PASS" });
        }
        await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
        continue;
      }

      if (value === "failed") {
        // failJobStep marks status=failed + delivers the error message;
        // saveJobSnapshotStep persists the XState snapshot. Independent
        // writes (different Redis keys), so concurrent.
        await Promise.all([
          failJobStep({
            jobId,
            error: ctx.errorText ?? "unknown failure",
          }),
          saveJobSnapshotStep({
            jobId,
            snapshot: actor.getPersistedSnapshot(),
          }),
        ]);
        return;
      }

      // Unknown state — fail safely instead of looping.
      actor.send({ type: "FAIL", error: `unknown state: ${value}` });
      await saveJobSnapshotStep({ jobId, snapshot: actor.getPersistedSnapshot() });
    }

    if (ticks >= MAX_TICKS) {
      await failJobStep({ jobId, error: `job exceeded MAX_TICKS=${MAX_TICKS}` });
      return;
    }

    const final = actor.getSnapshot();
    if (final.status === "done") {
      const result = final.context.executionResult;
      // finalize delivers the result to chat + marks the job done;
      // persistDeepJob writes the synthesis/subtasks/citations to VFS.
      // Independent (different sinks, no shared state) — run concurrently.
      //
      // Persist's error-swallow semantics are preserved by attaching the
      // catch on its promise directly, so any failure there resolves
      // silently and Promise.all rejects only if finalize itself throws
      // (which the outer try/catch already handles via failJobStep).
      await Promise.all([
        finalizeJobStep({
          jobId,
          tenantId: final.context.tenantId,
          sessionId: final.context.sessionId,
          channel: final.context.channel,
          resultText: result?.text ?? "",
          artifacts: result?.artifacts ?? [],
          alreadyDelivered: delivered,
        }),
        persistDeepJobStep({ jobId }).catch(() => {
          /* persistence failure is annoying but not fatal — the user
             already got the in-chat result; the artifact lives on in job
             meta even if the VFS write hiccupped. */
        }),
      ]);
    }
  } catch (err: any) {
    await failJobStep({
      jobId,
      error: err?.message ?? String(err),
    });
  }
}
