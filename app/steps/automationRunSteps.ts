// app/steps/automationRunSteps.ts
//
// Durable `"use step"` units used by app/workflows/automationWorkflow.ts. Each
// is a checkpointed operation: loading the run + its rule, executing a light
// step list, spinning up a full agent job, polling that job to completion, and
// finalizing (deliver summary + persist status).

import {
  getRun,
  getAutomation,
  patchRun,
  appendRunThought,
  summarizeEvent,
  type Automation,
  type AutomationRun,
  type LightStep,
  type PlanStep,
  type PlanValue,
} from "@/app/lib/automations";
import { getStore } from "@/app/lib/store";
import { createJob, getJobMeta, updateJobMeta, type JobMeta } from "@/app/lib/jobStore";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { recordActivity } from "@/app/lib/activityLog";
import { env } from "@/app/lib/env";
import { resolveModel, resolveModelName } from "@/app/lib/modelRouting";
import { executeComposioAction } from "@/app/lib/composioExec";
import { generateText, type LanguageModel } from "ai";

// Default model for non-deep, single-shot automation turns. These fire on a
// schedule/trigger with NO human watching, so a frequently-armed automation
// (e.g. an everyMs / "* * * * *" cron) running a flagship turn every minute is
// the dominant background spend. Non-deep automations are simple side-effect
// or summarize tasks — gpt-5.4-mini handles them at a fraction of the cost.
// Deep automations still take the full premium orchestration via jobWorkflow.
// Override per-deployment with AUTOMATION_TURN_MODEL.
function automationTurnModel(): string {
  return env("AUTOMATION_TURN_MODEL") ?? resolveModelName("fast-meta");
}

// --- load ---------------------------------------------------------------

export async function loadAutomationRunStep(runId: string): Promise<{
  run: AutomationRun | null;
  rule: Automation | null;
}> {
  "use step";
  const run = await getRun(runId);
  if (!run) return { run: null, rule: null };
  const rule = await getAutomation(run.automationId);
  return { run, rule };
}

// --- light mode ---------------------------------------------------------

// Minimal VFS write/append using the same Redis key scheme agentTurn uses, so
// files written here are visible to the agent + the VFS UI. Scoped to the
// rule's tenant (userId) + session.
function vfsNodeKey(userId: string, sessionId: string, path: string): string {
  return `vfs:${userId}:${sessionId}:node:${sanitizeVfsPath(path)}`;
}
function vfsPathsKey(userId: string, sessionId: string): string {
  return `vfs:${userId}:${sessionId}:paths`;
}
function sanitizeVfsPath(p: string): string {
  let out = ("/" + String(p)).replace(/\/+/g, "/").trim();
  if (out.length > 1 && out.endsWith("/")) out = out.slice(0, -1);
  return out || "/";
}

type VfsFileNode = {
  type: "file";
  path: string;
  content: string;
  createdAt: string;
  updatedAt: string;
};

async function vfsRead(userId: string, sessionId: string, path: string): Promise<string> {
  const node = await getStore().get<VfsFileNode>(vfsNodeKey(userId, sessionId, path));
  return node && node.type === "file" ? node.content : "";
}
async function vfsWrite(
  userId: string,
  sessionId: string,
  path: string,
  content: string
): Promise<void> {
  const store = getStore();
  const p = sanitizeVfsPath(path);
  const now = new Date().toISOString();
  const existing = await store.get<VfsFileNode>(vfsNodeKey(userId, sessionId, p));
  const node: VfsFileNode = {
    type: "file",
    path: p,
    content,
    createdAt: existing?.createdAt ?? now,
    updatedAt: now,
  };
  await store.set(vfsNodeKey(userId, sessionId, p), node);
  await store.sadd(vfsPathsKey(userId, sessionId), p);
}

export async function runLightStepsStep(args: { runId: string }): Promise<string> {
  "use step";
  const run = await getRun(args.runId);
  if (!run) throw new Error(`run not found: ${args.runId}`);
  const rule = await getAutomation(run.automationId);
  if (!rule || rule.action.mode !== "light") {
    throw new Error("runLightStepsStep called on a non-light automation");
  }

  const userId = rule.tenantId;
  const sessionId = rule.sessionId;
  const summary: string[] = [];

  for (const step of rule.action.steps as LightStep[]) {
    if (step.op === "send") {
      await sendOutboundRuntime({
        channel: rule.channel,
        sessionId,
        text: step.text,
      });
      await appendRunThought(args.runId, { kind: "step", text: `sent: ${step.text.slice(0, 80)}` });
      summary.push(`sent message`);
    } else if (step.op === "vfs_write") {
      await vfsWrite(userId, sessionId, step.path, step.content);
      await appendRunThought(args.runId, { kind: "step", text: `wrote ${step.path}` });
      summary.push(`wrote ${step.path}`);
    } else if (step.op === "vfs_append") {
      const cur = await vfsRead(userId, sessionId, step.path);
      await vfsWrite(userId, sessionId, step.path, cur + step.content);
      await appendRunThought(args.runId, { kind: "step", text: `appended ${step.path}` });
      summary.push(`appended ${step.path}`);
    }
  }

  return summary.length ? `Done: ${summary.join(", ")}.` : "Done (no steps).";
}

// --- plan mode (deterministic tool-call workflow) -----------------------

// The model that fills in `ai`-typed plan values (e.g. an email body). This is
// the ONLY LLM call a plan makes; deterministic const/event fields never touch
// a model. Overridable via AUTOMATION_GEN_MODEL; defaults to the smart slot so
// generated copy is decent. (Never DeepSeek — that's the chat slot only.)
function planGenModel(): LanguageModel {
  return resolveModel(env("AUTOMATION_GEN_MODEL") ?? resolveModelName("smart"));
}

// Dot-path lookup into the (possibly nested) triggering event payload.
function getByPath(obj: unknown, path: string): unknown {
  if (!path) return obj;
  let cur: unknown = obj;
  for (const key of path.split(".")) {
    if (cur == null || typeof cur !== "object") return undefined;
    cur = (cur as Record<string, unknown>)[key];
  }
  return cur;
}

// Compact, model-friendly view of the event that fired this run — the digest
// computed at fire time when available, else the (trimmed) raw payload. Given
// to `ai` values as context so "summarize this email" style generation works.
function planEventContext(run: AutomationRun): string {
  if (run.eventSummary?.fields) return run.eventSummary.fields;
  try {
    const s = typeof run.event === "string" ? run.event : JSON.stringify(run.event, null, 2);
    return s && s !== "null" && s !== '""' ? s.slice(0, 2000) : "";
  } catch {
    return "";
  }
}

async function resolvePlanValue(
  v: PlanValue,
  ctx: { event: unknown; eventText: string; model: LanguageModel }
): Promise<string> {
  if (v.from === "const") return v.value ?? "";
  if (v.from === "event") {
    const got = getByPath(ctx.event, v.path);
    if (got == null) return "";
    return typeof got === "string" ? got : JSON.stringify(got);
  }
  // v.from === "ai": a single generation call — the only model spend in a plan.
  const { text } = await generateText({
    model: ctx.model,
    prompt:
      `${v.prompt}\n\n` +
      (ctx.eventText
        ? `Context — the event that triggered this automation:\n${ctx.eventText}\n\n`
        : "") +
      "Return only the requested content itself — no preamble, labels, or surrounding quotes.",
  });
  return text.trim();
}

// Execute a deterministic plan: a fixed list of send / VFS / Composio steps
// whose parameters were bound at compile time. Runs the tools directly (no
// agent, no tool-search) and only calls the LLM for `ai`-typed values. One
// durable step so the whole plan checkpoints/replays as a unit.
export async function runPlanStepsStep(args: { runId: string }): Promise<string> {
  "use step";
  const run = await getRun(args.runId);
  if (!run) throw new Error(`run not found: ${args.runId}`);
  const rule = await getAutomation(run.automationId);
  if (!rule || rule.action.mode !== "plan") {
    throw new Error("runPlanStepsStep called on a non-plan automation");
  }

  const userId = rule.tenantId;
  const sessionId = rule.sessionId;
  const ctx = {
    event: run.event,
    eventText: planEventContext(run),
    // Built lazily-ish: constructing the model is cheap and a plan with no `ai`
    // values simply never calls generateText, so no tokens are spent.
    model: planGenModel(),
  };
  const summary: string[] = [];

  for (const step of rule.action.steps as PlanStep[]) {
    if (step.op === "send") {
      const text = await resolvePlanValue(step.text, ctx);
      await sendOutboundRuntime({ channel: rule.channel, sessionId, text });
      await appendRunThought(args.runId, { kind: "step", text: `sent: ${text.slice(0, 80)}` });
      summary.push("sent message");
    } else if (step.op === "vfs_write") {
      const content = await resolvePlanValue(step.content, ctx);
      await vfsWrite(userId, sessionId, step.path, content);
      await appendRunThought(args.runId, { kind: "step", text: `wrote ${step.path}` });
      summary.push(`wrote ${step.path}`);
    } else if (step.op === "vfs_append") {
      const content = await resolvePlanValue(step.content, ctx);
      const cur = await vfsRead(userId, sessionId, step.path);
      await vfsWrite(userId, sessionId, step.path, cur + content);
      await appendRunThought(args.runId, { kind: "step", text: `appended ${step.path}` });
      summary.push(`appended ${step.path}`);
    } else if (step.op === "composio") {
      const argsObj: Record<string, unknown> = {};
      for (const [k, val] of Object.entries(step.args)) {
        argsObj[k] = await resolvePlanValue(val, ctx);
      }
      const res = await executeComposioAction(userId, step.tool, argsObj, step.connectedAccountId);
      if (!res.ok) {
        await appendRunThought(args.runId, {
          kind: "error",
          text: `${step.tool} failed: ${(res.error ?? "unknown").slice(0, 160)}`,
        });
        throw new Error(`composio ${step.tool} failed: ${res.error ?? "unknown"}`);
      }
      await appendRunThought(args.runId, { kind: "step", text: `ran ${step.tool}` });
      summary.push(`ran ${step.tool}`);
    }
  }

  return summary.length ? `Done: ${summary.join(", ")}.` : "Done (no steps).";
}

// Seed (or overwrite) an automation's durable state file. Used to point a
// recurring automation at a resource it already created on an earlier run
// (e.g. an existing spreadsheet id) so the next firing reuses it instead of
// creating a fresh one. Writes to the same per-tenant/session VFS namespace
// the agent reads from.
export async function seedAutomationState(
  automationId: string,
  content: string
): Promise<{ tenantId: string; sessionId: string; path: string } | null> {
  const rule = await getAutomation(automationId);
  if (!rule) return null;
  const path = `/automations/${automationId}/state.json`;
  await vfsWrite(rule.tenantId, rule.sessionId, path, content);
  return { tenantId: rule.tenantId, sessionId: rule.sessionId, path };
}

// Read an automation's durable state file (debug/inspection).
export async function readAutomationState(
  automationId: string
): Promise<{ tenantId: string; sessionId: string; path: string; content: string } | null> {
  const rule = await getAutomation(automationId);
  if (!rule) return null;
  const path = `/automations/${automationId}/state.json`;
  const content = await vfsRead(rule.tenantId, rule.sessionId, path);
  return { tenantId: rule.tenantId, sessionId: rule.sessionId, path, content };
}

// --- job mode -----------------------------------------------------------

// Build a self-contained prompt for the agent job: the rule instruction plus
// the triggering event payload (so the agent can act on it) plus an explicit
// skill-load directive when the rule named skills.
function buildJobPrompt(rule: Automation, run: AutomationRun): string {
  if (rule.action.mode !== "job") return "";
  const parts: string[] = [];

  // Lead with an unambiguous execution directive. Without this, the agent
  // reads a task instruction plus a skill list as a request to AUTHOR an
  // automation handler — and replies with a JavaScript function (e.g.
  // `export default async function handler({ event, skills })`) instead of
  // doing the work. The user then gets source code in chat and a false
  // "finished", with nothing actually executed.
  //
  // Deliberately tool-AGNOSTIC: it names no app, tool, or resource type. The
  // task instruction below carries the intent; the agent discovers and uses the
  // right tools dynamically, so this works identically for every integration.
  parts.push(
    "You are running an automation that just fired. PERFORM the task below " +
      "right now by calling your own live tools against the event payload. " +
      "Discover and use whichever tools the task actually needs — your " +
      "integration/app tools, file tools, etc. Carry out the real side effects " +
      "yourself this turn.\n\n" +
      "Do exactly what the task specifies, using the exact app, resource, and " +
      "output format it names — never substitute a different tool or resource " +
      "type for the one requested. If the task doesn't name a specific one, pick " +
      "the tool that best fits the job.\n\n" +
      "Do NOT write, print, or send source code, a handler function, or " +
      "pseudo-code that describes how the automation WOULD work — that is not " +
      "the deliverable and counts as a failure. There is no `skills` object to " +
      "import and no serverless handler to author; you act directly through " +
      "your tools. When done, reply with a short plain-language summary of what " +
      "you actually did (the concrete resource ids/links you touched and what " +
      "you changed), not code. If a tool or app the task needs isn't available " +
      "or connected, say so plainly and stop — do not fabricate success."
  );

  // Persistence + idempotency. This automation fires repeatedly, each time as a
  // FRESH agent with no memory of prior runs — so without an explicit,
  // deterministic state file it re-creates resources every run and overwrites
  // earlier data instead of appending. Pin a stable per-automation state path
  // and spell out reuse + append semantics — again with no tool-specific terms.
  const statePath = `/automations/${rule.id}/state.json`;
  parts.push(
    "\nPERSISTENCE & IDEMPOTENCY (this automation runs many times; each run is " +
      "a fresh agent with NO memory of previous runs):\n" +
      `- Your durable state file is \`${statePath}\` on the VFS. FIRST, read it. ` +
      "It holds the ids/handles of any resources you created on earlier runs.\n" +
      "- REUSE, do not recreate: if the state file already names the resource " +
      "this task uses, use that exact id/handle. Only create a new one when the " +
      "state file is missing or has no id for it — and immediately write the new " +
      "id back to the state file so the next run reuses it. Never create a " +
      "duplicate resource when one already exists.\n" +
      "- ADD, do not overwrite: append this run's output AFTER the existing " +
      "content, using whatever append/insert operation the target tool provides. " +
      "Never clear, replace, or overwrite content you didn't add this run.\n" +
      "- ACTUALLY WRITE THE OUTPUT: populate the target with the real data this " +
      "run produced. Creating or opening an empty/placeholder target and " +
      "stopping is a FAILURE — confirm the write landed before reporting " +
      "success.\n" +
      "- Dedupe correctly (only when the task logs/collects items): skip as " +
      "'already handled' only when the CURRENT event's unique id is genuinely " +
      "already present in the target. Take the id from THIS run's event payload " +
      "below, not from the target or the state file."
  );

  parts.push("\nTask:");
  parts.push(rule.action.instruction.trim());
  if (rule.action.skills && rule.action.skills.length) {
    // Skills are reference knowledge consulted via list_skills/read_skill —
    // NOT importable modules. Phrasing matters: "load these" invited the
    // code-authoring misread above.
    parts.push(
      `\nIf helpful, consult these skills for guidance (via your skill tools): ${rule.action.skills.join(", ")}.`
    );
  }
  // Surface the event's identity + display fields explicitly. The raw Gmail
  // payload is a huge blob (full MIME headers, ARC seals, base64 attachments)
  // and after truncation the agent often can't locate the real message_id —
  // so it grabs an id from the spreadsheet's last row instead and wrongly
  // skips genuinely new emails as "already present". Pulling the key fields
  // out and pinning the CURRENT id up front removes that ambiguity.
  // Prefer the digest computed at fire time on the full (untrimmed) event;
  // fall back to summarizing whatever event we have stored.
  const summary = run.eventSummary ?? summarizeEvent(run.event);
  if (summary.id) {
    parts.push(
      `\nCURRENT EVENT ID: ${summary.id}\n` +
        "This is the unique id of the item this run must process. When deciding " +
        "whether it is a duplicate, compare THIS id against the ids already in " +
        "the target resource — do NOT treat an id you read from the resource or " +
        "state file as the current item."
    );
  }
  if (summary.fields) {
    parts.push(
      `\nKey fields from this event (source: ${run.source}):\n\`\`\`json\n${summary.fields}\n\`\`\``
    );
  }
  let eventStr = "";
  try {
    eventStr = JSON.stringify(run.event, null, 2);
  } catch {
    eventStr = String(run.event);
  }
  if (eventStr && eventStr !== "null" && eventStr !== '""') {
    parts.push(
      `\nFull raw event payload (for reference only — prefer the key fields above):\n\n\`\`\`json\n${eventStr.slice(0, 4000)}\n\`\`\``
    );
  }
  return parts.join("\n");
}

export async function createAutomationJobStep(args: { runId: string }): Promise<string> {
  "use step";
  const run = await getRun(args.runId);
  if (!run) throw new Error(`run not found: ${args.runId}`);
  const rule = await getAutomation(run.automationId);
  if (!rule || rule.action.mode !== "job") {
    throw new Error("createAutomationJobStep called on a non-job automation");
  }

  const prompt = buildJobPrompt(rule, run);
  const meta = await createJob({
    tenantId: rule.tenantId,
    channel: rule.channel,
    sessionId: rule.sessionId,
    prompt,
    // deep automations take the research/orchestrating branch; otherwise a
    // single focused agent turn ("auto").
    kind: rule.action.deep ? "research" : "auto",
  });

  await patchRun(args.runId, { jobId: meta.jobId });

  // Launch the job's durable workflow from inside this step — the WDK forbids
  // calling start() from a workflow body, so it must happen here. Dynamic
  // import keeps the automations module free of a static workflow dependency.
  const { start } = await import("workflow/api");
  const { jobWorkflow } = await import("@/app/workflows/jobWorkflow");
  await start(jobWorkflow, [meta.jobId]);

  await appendRunThought(args.runId, {
    kind: "step",
    text: `launched ${rule.action.deep ? "deep" : "normal"} job ${meta.jobId}`,
  });
  return meta.jobId;
}

// Prepare a SINGLE-SHOT automation job: create the job record + build the
// prompt, but do NOT start jobWorkflow. The caller (automationWorkflow) then
// runs exactly one executeAgentTurnStep against it — no clarify/plan/verify/
// revise loop. This matters because that loop re-runs the executor on every
// revise pass, and for a side-effecting action (e.g. "append a row to the
// sheet") each re-execution repeats the side effect: the verifier reads a
// "skipped, already logged" turn as "didn't do the work", sends it back to
// execute, and the email lands in A8 AND A9 while the user gets ~7 messages.
// One turn = one side effect = one message. Deep automations still take the
// full orchestration via createAutomationJobStep.
export async function prepareAutomationTurnStep(args: { runId: string }): Promise<{
  jobId: string;
  tenantId: string;
  sessionId: string;
  channel: Automation["channel"];
  prompt: string;
  modelName: string;
}> {
  "use step";
  const run = await getRun(args.runId);
  if (!run) throw new Error(`run not found: ${args.runId}`);
  const rule = await getAutomation(run.automationId);
  if (!rule || rule.action.mode !== "job") {
    throw new Error("prepareAutomationTurnStep called on a non-job automation");
  }
  const prompt = buildJobPrompt(rule, run);
  const meta = await createJob({
    tenantId: rule.tenantId,
    channel: rule.channel,
    sessionId: rule.sessionId,
    prompt,
    kind: "auto",
  });
  await patchRun(args.runId, { jobId: meta.jobId });
  await appendRunThought(args.runId, {
    kind: "step",
    text: `single-shot job ${meta.jobId}`,
  });
  return {
    jobId: meta.jobId,
    tenantId: rule.tenantId,
    sessionId: rule.sessionId,
    channel: rule.channel,
    prompt,
    modelName: automationTurnModel(),
  };
}

// Mark a single-shot automation job terminal after its one agent turn ran.
export async function finishAutomationTurnStep(args: {
  jobId: string;
  text: string;
}): Promise<void> {
  "use step";
  await updateJobMeta(args.jobId, { status: "done", resultText: args.text });
}

// One poll of the linked job's status. The workflow body sleeps (WDK durable
// sleep) between calls; this step just reads the current meta so each poll is
// a checkpoint.
export async function pollJobStep(args: { jobId: string }): Promise<{
  status: JobMeta["status"];
  terminal: boolean;
  resultText?: string;
  error?: string;
}> {
  "use step";
  const meta = await getJobMeta(args.jobId);
  if (!meta) return { status: "failed", terminal: true, error: "job meta missing" };
  const terminal =
    meta.status === "done" || meta.status === "failed" || meta.status === "cancelled";
  return {
    status: meta.status,
    terminal,
    resultText: meta.resultText,
    error: meta.error,
  };
}

// --- finalize -----------------------------------------------------------

export async function finalizeAutomationRunStep(args: {
  runId: string;
  status: "ok" | "error";
  resultText?: string;
  error?: string;
}): Promise<void> {
  "use step";
  const run = await getRun(args.runId);
  if (!run) return;
  const rule = await getAutomation(run.automationId);

  await patchRun(args.runId, {
    status: args.status,
    resultText: args.resultText,
    error: args.error,
    finishedAt: Date.now(),
  });

  // Mark the rule itself "error" if this run failed, so the dashboard surfaces
  // a broken automation; leave it "active" on success.
  if (rule) {
    if (args.status === "error" && rule.enabled) {
      await patchRuleStatus(rule.id, "error");
    } else if (args.status === "ok" && rule.status === "error" && rule.enabled) {
      await patchRuleStatus(rule.id, "active");
    }
  }

  // Deliver a short summary to the user's channel. For job-mode runs the job
  // already delivered its own final answer, so we send a compact header; for
  // light runs this is the only delivery.
  if (rule) {
    const name = rule.name || "Automation";
    const body =
      args.status === "ok"
        ? // Light, plan, and workforce runs deliver their result text here
          // (nothing else delivers it); job runs already delivered their answer.
          rule.action.mode === "light" ||
          rule.action.mode === "plan" ||
          rule.action.mode === "workforce"
          ? args.resultText || "done"
          : `✅ automation "${name}" finished.`
        : `⚠️ automation "${name}" hit an error: ${(args.error || "unknown").slice(0, 200)}`;
    try {
      await sendOutboundRuntime({
        channel: rule.channel,
        sessionId: rule.sessionId,
        text: body,
      });
    } catch {
      // outbound failure shouldn't fail the run record
    }
    await recordActivity(rule.tenantId, {
      kind: "automation",
      summary: `${name}: ${args.status}`,
      meta: { automationId: rule.id, runId: args.runId, jobId: run.jobId, status: args.status },
    });
  }
}

async function patchRuleStatus(
  ruleId: string,
  status: Automation["status"]
): Promise<void> {
  const rule = await getAutomation(ruleId);
  if (!rule) return;
  rule.status = status;
  await getStore().set(`auto:rule:${ruleId}`, rule);
}
