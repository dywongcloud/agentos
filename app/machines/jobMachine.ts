// app/machines/jobMachine.ts
//
// XState v5 machine for one long-running agent job.
//
// Important: this machine does NOT itself perform async work via `invoke`.
// The Vercel Workflow runner (app/workflows/jobWorkflow.ts) reads the current
// state, executes a WDK durable step, and sends the result back as an event.
// That keeps the entire computation snapshot-restorable across cron resumptions
// and Fluid Compute function boundaries — XState owns the state graph, WDK
// owns the durability.
//
// State graph (slice 1):
//
//   pending → clarifying → planning → executing → verifying → done
//                  ↓ (ASK)                          ↓ (REVISE)
//             needs_input ─── (RESUME) ───→ planning ⟵
//
//   any state → failed   (on FAIL)
//
// Slice 1 reality:
//   - clarifying / planning / verifying immediately pass through with stub
//     events. They are real states (not skeleton holes) — slice 2/3 fills the
//     stub actors with real logic without changing the graph.
//   - executing is the only state that does meaningful work; runner invokes
//     agentTurn() and sends EXECUTE_DONE back.

import { setup, assign, type AnyEventObject } from "xstate";

import type { ModalityId } from "@/app/lib/rubrics";

export type JobMachineInput = {
  jobId: string;
  tenantId: string;
  sessionId: string;
  channel: "telegram" | "whatsapp" | "sms";
  prompt: string;
  // "deep" jobs route through the orchestrator loop (research + execute
  // subtasks driven by reasoning-pro). "normal" jobs use the simpler
  // plan → execute → verify path.
  kind: "normal" | "deep";
};

// One subtask run inside a deep-mode orchestrator loop.
export type SubtaskResult = {
  id: string;
  iter: number;
  kind: "research" | "execute" | "synthesize";
  goal: string;
  output: string; // text summary suitable for the orchestrator's next decision
  artifacts: string[];
  citations: string[];
  ts: number;
};

export type JobMachineContext = JobMachineInput & {
  assumptions: string[];
  plan: PlanStep[];
  modality: ModalityId | null;
  // Used by deep-mode orchestrator; empty for normal jobs.
  subtaskResults: SubtaskResult[];
  finalSynthesis: string | null;
  // Set when execute / orchestrator commits a final user-facing text answer.
  executionResult: { text: string; artifacts: string[] } | null;
  verifierNotes: string[];
  reviseCount: number;
  pendingQuestion: string | null;
  errorText: string | null;
};

export type PlanStep = {
  id: string;
  description: string;
  kind: "research" | "code" | "write" | "tool" | "verify";
};

export type JobMachineEvent =
  // clarification phase
  | { type: "CLARIFY_DONE"; assumptions: string[] }
  | { type: "ASK"; question: string }
  | { type: "RESUME"; answer: string }
  // planning phase (normal jobs only)
  | { type: "PLAN_DONE"; plan: PlanStep[]; modality: ModalityId }
  // execution phase (normal jobs)
  | {
      type: "EXECUTE_DONE";
      text: string;
      artifacts: string[];
    }
  // deep-mode orchestrator events
  | { type: "SUBTASK_DONE"; result: SubtaskResult }
  | {
      type: "ORCHESTRATE_FINAL";
      finalText: string;
      artifacts: string[];
      modality: ModalityId;
    }
  // verification phase
  | { type: "VERIFY_PASS" }
  | { type: "VERIFY_REVISE"; notes: string[] }
  // global
  | { type: "FAIL"; error: string }
  | { type: "CANCEL" };

// Total revise loops (correctness revisions + depth-driven extra passes share
// this budget). Bumped 3→6 so the depth reviewer can push for several deeper
// passes before the machine force-accepts. The dollar budget + DEEP_MAX_DEPTH
// _PASSES are the real governors; this is the hard safety bound.
export const MAX_REVISE_PASSES = Number(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).process?.env?.DEEP_MAX_REVISE_PASSES ?? "6"
) || 6;

export const jobMachine = setup({
  types: {
    input: {} as JobMachineInput,
    context: {} as JobMachineContext,
    events: {} as JobMachineEvent,
  },
  guards: {
    canRevise: ({ context }) => context.reviseCount < MAX_REVISE_PASSES,
    isDeep: ({ context }) => context.kind === "deep",
    isNormal: ({ context }) => context.kind !== "deep",
  },
  actions: {
    setAssumptions: assign({
      assumptions: ({ event }) =>
        event.type === "CLARIFY_DONE" ? event.assumptions : [],
    }),
    setPlan: assign({
      plan: ({ event }) => (event.type === "PLAN_DONE" ? event.plan : []),
      modality: ({ event, context }) =>
        event.type === "PLAN_DONE" ? event.modality : context.modality,
    }),
    setExecutionResult: assign({
      executionResult: ({ event }) =>
        event.type === "EXECUTE_DONE"
          ? { text: event.text, artifacts: event.artifacts }
          : null,
    }),
    appendSubtaskResult: assign({
      subtaskResults: ({ context, event }) =>
        event.type === "SUBTASK_DONE"
          ? [...context.subtaskResults, event.result]
          : context.subtaskResults,
    }),
    setOrchestratedFinal: assign({
      executionResult: ({ event }) =>
        event.type === "ORCHESTRATE_FINAL"
          ? { text: event.finalText, artifacts: event.artifacts }
          : null,
      finalSynthesis: ({ event }) =>
        event.type === "ORCHESTRATE_FINAL" ? event.finalText : null,
      modality: ({ event, context }) =>
        event.type === "ORCHESTRATE_FINAL" ? event.modality : context.modality,
    }),
    setVerifierNotes: assign({
      verifierNotes: ({ event }) =>
        event.type === "VERIFY_REVISE" ? event.notes : [],
    }),
    bumpReviseCount: assign({
      reviseCount: ({ context }) => context.reviseCount + 1,
    }),
    setPendingQuestion: assign({
      pendingQuestion: ({ event }) =>
        event.type === "ASK" ? event.question : null,
    }),
    clearPendingQuestion: assign({ pendingQuestion: () => null }),
    setError: assign({
      errorText: ({ event }: { event: AnyEventObject }) =>
        event.type === "FAIL" ? String(event.error) : "cancelled",
    }),
  },
}).createMachine({
  id: "job",
  initial: "pending",
  context: ({ input }) => ({
    jobId: input.jobId,
    tenantId: input.tenantId,
    sessionId: input.sessionId,
    channel: input.channel,
    prompt: input.prompt,
    kind: input.kind,
    assumptions: [],
    plan: [],
    modality: null,
    subtaskResults: [],
    finalSynthesis: null,
    executionResult: null,
    verifierNotes: [],
    reviseCount: 0,
    pendingQuestion: null,
    errorText: null,
  }),
  on: {
    FAIL: { target: ".failed", actions: "setError" },
    CANCEL: { target: ".failed", actions: "setError" },
  },
  states: {
    pending: {
      always: { target: "clarifying" },
    },

    clarifying: {
      on: {
        CLARIFY_DONE: [
          // Deep jobs skip planning + executing and enter the orchestrator
          // loop directly.
          {
            target: "orchestrating",
            guard: "isDeep",
            actions: "setAssumptions",
          },
          { target: "planning", actions: "setAssumptions" },
        ],
        ASK: { target: "needs_input", actions: "setPendingQuestion" },
      },
    },

    needs_input: {
      on: {
        RESUME: [
          {
            target: "orchestrating",
            guard: "isDeep",
            actions: "clearPendingQuestion",
          },
          { target: "clarifying", actions: "clearPendingQuestion" },
        ],
      },
    },

    planning: {
      on: {
        PLAN_DONE: { target: "executing", actions: "setPlan" },
      },
    },

    orchestrating: {
      // The runner drives this state with multiple subtask iterations.
      // SUBTASK_DONE accumulates results in context; ORCHESTRATE_FINAL
      // commits the final user-facing text and moves to verification.
      on: {
        SUBTASK_DONE: { actions: "appendSubtaskResult" },
        ORCHESTRATE_FINAL: {
          target: "verifying",
          actions: "setOrchestratedFinal",
        },
      },
    },

    executing: {
      on: {
        EXECUTE_DONE: { target: "verifying", actions: "setExecutionResult" },
      },
    },

    verifying: {
      on: {
        VERIFY_PASS: { target: "done" },
        VERIFY_REVISE: [
          // Revise → back to the right re-planning state for the job kind.
          {
            target: "orchestrating",
            guard: ({ context }) =>
              context.kind === "deep" &&
              context.reviseCount < MAX_REVISE_PASSES,
            actions: ["setVerifierNotes", "bumpReviseCount"],
          },
          {
            target: "planning",
            guard: ({ context }) =>
              context.kind !== "deep" &&
              context.reviseCount < MAX_REVISE_PASSES,
            actions: ["setVerifierNotes", "bumpReviseCount"],
          },
          // out of revise budget — accept current output rather than infinite-loop
          { target: "done", actions: "setVerifierNotes" },
        ],
      },
    },

    done: { type: "final" },
    failed: { type: "final" },
  },
});

