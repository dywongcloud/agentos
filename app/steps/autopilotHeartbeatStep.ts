// app/steps/autopilotHeartbeatStep.ts
//
// One heartbeat tick for one tenant. Runs every minute from the daemon cron.
//
// Default behavior: do nothing. The autopilot only reaches out when there's
// something genuinely worth saying — a job completed, a long-running task
// hit a milestone, the user hasn't been pinged about a known reminder, etc.
//
// Flow:
//   1. Deterministic preflight (checkProactiveGate): bail on opt-out,
//      quiet hours, cooldown, recent user activity. Zero LLM cost in the
//      common case.
//   2. Gather a compact context blob: recent activity, active jobs,
//      subscriptions, recent memories, last proactive reason.
//   3. Ask a small model (gpt-4.1-mini by default) with structured output:
//      { should_message, message, reason, importance }. Default is `false`.
//   4. If should_message: deliver via the tenant's last-known Telegram
//      session and record the cooldown timestamp + reason.
//
// Cost guardrails:
//   - Most ticks short-circuit at step 1 (no LLM call at all).
//   - The model call is bounded — gpt-4.1-mini, ~1k tokens in / ~200 out
//     ≈ $0.001 per tick.
//   - With 1-min cron and 1 tenant opted in, max ~$1.50/day per tenant.

import { generateObject } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";
import { z } from "zod/v4";

import { env } from "@/app/lib/env";
import { recordActivity, listActivity } from "@/app/lib/activityLog";
import { listRecent } from "@/app/lib/memoryStore";
import { listActiveJobs, listRecentJobs, getJobMeta } from "@/app/lib/jobStore";
import { getSessionMeta, getLastSession } from "@/app/lib/sessionMeta";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { listSubscriptions } from "@/app/lib/composioTriggers";
import {
  checkProactiveGate,
  recordProactiveSent,
  getLastProactive,
  getLastProactiveReason,
  getHeartbeatLlmState,
  recordHeartbeatLlm,
  heartbeatMaxIdleMs,
} from "@/app/lib/autopilotProactive";

// gpt-4.1-mini is the cheap workhorse for this loop — a binary "should I ping?"
// classifier doesn't need the full model, and this loop runs in the background
// at most every few minutes. Env (AUTOPILOT_HEARTBEAT_MODEL / FAST_META_MODEL_NAME)
// overrides if a tenant wants to escalate.
const HEARTBEAT_MODEL_DEFAULT = "gpt-4.1-mini";

const decisionSchema = z.object({
  should_message: z.boolean(),
  message: z
    .string()
    .nullable()
    .describe(
      "What to send, in your own voice. Casual, lowercase, ≤320 chars, no buzzwords. null when should_message=false."
    ),
  reason: z
    .string()
    .min(1)
    .max(300)
    .describe(
      "Short rationale — logged for debugging. Visible to the user via /autopilot status."
    ),
  importance: z.enum(["low", "medium", "high"]),
});

async function findLastUserMessageMs(tenantId: string): Promise<number | null> {
  // Heuristic: walk recent activity for the "agentTurn started" entries
  // we record at the top of every agentTurn invocation. Their timestamps
  // approximate the user's last message.
  const entries = await listActivity(tenantId, { limit: 40 });
  const recentTurn = entries.find(
    (e) => e.kind === "tool" && e.summary.startsWith("agentTurn started")
  );
  return recentTurn?.ts ?? null;
}

function shorten(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}

async function buildContext(
  tenantId: string,
  lastUserMs: number | null
): Promise<{ text: string; fp: string }> {
  const [activities, recentMems, activeJobIds, recentJobIds, subs, lastSent, lastReason] =
    await Promise.all([
      listActivity(tenantId, { limit: 20 }),
      listRecent(tenantId, 6),
      listActiveJobs(tenantId),
      listRecentJobs(tenantId, 6),
      listSubscriptions(tenantId),
      getLastProactive(tenantId),
      getLastProactiveReason(tenantId),
    ]);

  // Hydrate a few recent jobs for status/result preview.
  const recentJobs: Array<{ id: string; status: string; resultPreview?: string }> = [];
  for (const id of recentJobIds.slice(0, 4)) {
    const meta = await getJobMeta(id);
    if (!meta) continue;
    recentJobs.push({
      id,
      status: meta.status,
      resultPreview: meta.resultText ? shorten(meta.resultText, 140) : undefined,
    });
  }

  // Fingerprint of the OBSERVABLE state the model reasons about — job
  // activity, subscription set, and the user's last message. Deliberately
  // excludes wall-clock and the heartbeat's own log entries so an idle tick
  // hashes identically minute over minute and the LLM call is skipped.
  const fp = [
    `aj:${[...activeJobIds].sort().join(",")}`,
    `rj:${recentJobs.map((j) => `${j.id}:${j.status}`).join(",")}`,
    `subs:${subs.length}`,
    `u:${lastUserMs ?? 0}`,
  ].join("|");

  const lines: string[] = [];
  lines.push(`Now: ${new Date().toISOString()}`);
  lines.push(
    `Last proactive: ${
      lastSent ? new Date(lastSent).toISOString() + ` (reason: ${lastReason ?? "?"})` : "never"
    }`
  );
  lines.push(
    `Active jobs: ${activeJobIds.length ? activeJobIds.join(", ") : "none"}`
  );
  if (recentJobs.length) {
    lines.push("Recent jobs:");
    for (const j of recentJobs) {
      lines.push(
        `  • ${j.id}  status=${j.status}${j.resultPreview ? `  result="${j.resultPreview}"` : ""}`
      );
    }
  }
  if (subs.length) {
    lines.push("Active triggers:");
    for (const s of subs.slice(0, 6)) {
      lines.push(`  • ${s.triggerName} (id ${s.triggerId})`);
    }
  }
  if (recentMems.length) {
    lines.push("Top memories (most-recent):");
    for (const m of recentMems) {
      lines.push(`  • [${m.kind}] ${m.title}`);
    }
  }
  if (activities.length) {
    lines.push("Recent activity (newest first):");
    for (const a of activities.slice(0, 10)) {
      const t = new Date(a.ts).toISOString().slice(11, 19);
      lines.push(`  ${t} [${a.kind}] ${shorten(a.summary, 140)}`);
    }
  }
  return { text: lines.join("\n"), fp };
}

export type HeartbeatResult =
  | { sent: false; reason: string }
  | { sent: true; message: string; reason: string; importance: string };

export async function autopilotHeartbeatStep(args: {
  tenantId: string;
}): Promise<HeartbeatResult> {
  "use step";

  const lastUserMs = await findLastUserMessageMs(args.tenantId);
  const gate = await checkProactiveGate({
    tenantId: args.tenantId,
    lastUserMessageMs: lastUserMs,
  });
  // Visible in Vercel logs so we can debug "why isn't autopilot firing" by
  // tailing the cron output. Cheap: one console line per tick per tenant.
  console.log(
    `[heartbeat] tenant=${args.tenantId} gate=${gate.allowed ? "open" : `closed:${(gate as any).reason}`}`
  );
  if (!gate.allowed) {
    return { sent: false, reason: `gate:${gate.reason}` };
  }

  const ctx = await buildContext(args.tenantId, lastUserMs);

  // Deterministic short-circuit: only spend a token when the observable state
  // actually changed since the last LLM decision, or a long idle interval has
  // elapsed (so deferred memory reminders still get a periodic look). Without
  // this the gate stays open every minute on an idle day and we'd pay for a
  // "should I message?" prompt ~900×/day that almost always answers "no".
  const { fp: lastFp, lastLlmMs } = await getHeartbeatLlmState(args.tenantId);
  const idleMs = lastLlmMs == null ? Infinity : Date.now() - lastLlmMs;
  const unchanged = lastFp != null && lastFp === ctx.fp;
  if (unchanged && idleMs < heartbeatMaxIdleMs()) {
    return { sent: false, reason: "no_state_change" };
  }
  // We're about to consult the model — stamp the decision time + fingerprint so
  // the next idle ticks short-circuit until something moves.
  await recordHeartbeatLlm(args.tenantId, ctx.fp);

  const model =
    env("AUTOPILOT_HEARTBEAT_MODEL") ??
    env("FAST_META_MODEL_NAME") ??
    HEARTBEAT_MODEL_DEFAULT;

  const system = [
    "You're the autopilot — a friend texting from inside an agent. Once a",
    "minute you peek at what's been happening (jobs, triggers, memories,",
    "recent chat) and decide: do I have something worth saying right now?",
    "",
    "Bias: lean slightly TOWARD reaching out when there's a real hook, but",
    "stay relaxed about silence. The user opted into this; they want to hear",
    "from you when it matters. They do NOT want filler.",
    "",
    "Good reasons to message:",
    "  - a job/deep-task they kicked off finished, errored, or hit a real",
    "    milestone — share the actual takeaway, not just 'it's done'",
    "  - a project (/code, /job, /deep) has been awaiting their input for",
    "    a while and they probably forgot",
    "  - a trigger/event came in that wasn't already auto-delivered",
    "  - it's been hours/a day since the last chat and they previously asked",
    "    you to follow up on something specific (a reminder in memory, a",
    "    deferred task, a 'check back tomorrow') — surface that hook by name",
    "  - they mentioned something time-sensitive earlier that's now relevant",
    "",
    "Don't message because:",
    "  - 'just checking in' / 'how's it going' (nope, never)",
    "  - the weather, the day of the week, motivational stuff",
    "  - to recap something they already saw",
    "  - the same reason as the last proactive message (look at",
    "    `Last proactive: …` in the snapshot — don't repeat yourself)",
    "",
    "Voice: casual texting, lowercase ok, contractions encouraged. No emojis",
    "unless they really fit. No bullet points in the message. Talk like a",
    "real person who knows the user — 'hey, that scrape job finished and",
    "the answer is X' beats 'Your job j_abc has completed successfully'.",
    "Keep it ≤320 chars. One thought, not three.",
    "",
    "The `reason` field is internal — explain to the engineer reading the",
    "logs why you chose what you chose. Be specific.",
  ].join("\n");

  let decision: z.infer<typeof decisionSchema>;
  try {
    const { object } = await generateObject({
      model: textAuxModel(model),
      schema: decisionSchema,
      system,
      prompt: `Tenant: ${args.tenantId}\n\nSnapshot:\n${ctx.text}`,
      temperature: 0.2,
    });
    decision = object;
  } catch (err: any) {
    await recordActivity(args.tenantId, {
      kind: "system",
      summary: `heartbeat decision failed: ${err?.message ?? String(err)}`,
    });
    return { sent: false, reason: `decision_error:${err?.message ?? "unknown"}` };
  }

  if (!decision.should_message || !decision.message) {
    // Log the skip reason occasionally for visibility, but skip the common
    // "nothing to say" case to avoid log noise.
    if (decision.reason && !/nothing|no change|quiet/i.test(decision.reason)) {
      await recordActivity(args.tenantId, {
        kind: "system",
        summary: `heartbeat: skip — ${shorten(decision.reason, 200)}`,
      });
    }
    return { sent: false, reason: decision.reason };
  }

  // Find a Telegram session to deliver into. tenantId is shape
  // "<channel>:<senderId>".
  const colon = args.tenantId.indexOf(":");
  const channel = colon > 0 ? args.tenantId.slice(0, colon) : "telegram";
  const senderPart = colon > 0 ? args.tenantId.slice(colon + 1) : args.tenantId;
  const sessMeta =
    (await getSessionMeta(`${channel}:${senderPart}`)) ??
    (await getLastSession(channel as never).then((l) =>
      l ? getSessionMeta(l.sessionId) : null
    ));
  if (!sessMeta) {
    return { sent: false, reason: "no_delivery_session" };
  }

  // No emoji prefix — the message itself is the voice. The previous "🤖" was
  // exactly the kind of robotic tell the persona refactor is trying to kill.
  await sendOutboundRuntime({
    channel: sessMeta.channel,
    sessionId: sessMeta.sessionId,
    text: decision.message,
  });
  console.log(
    `[heartbeat] tenant=${args.tenantId} SENT (${decision.importance}): ${shorten(decision.message, 120)}`
  );

  await recordProactiveSent(args.tenantId, decision.reason);
  await recordActivity(args.tenantId, {
    kind: "system",
    summary: `heartbeat → sent (${decision.importance}): ${shorten(decision.message, 200)}`,
    meta: { reason: decision.reason, importance: decision.importance },
  });

  return {
    sent: true,
    message: decision.message,
    reason: decision.reason,
    importance: decision.importance,
  };
}
