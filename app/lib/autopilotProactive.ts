// app/lib/autopilotProactive.ts
//
// Per-tenant opt-in store + cooldown / quiet-hours gating for the
// proactive autopilot heartbeat.
//
// Why a separate store from the existing autopilot system (autopilotState):
//   - autopilotState is global (one "primary" target, one enabled flag) —
//     fine for the legacy task-runner, wrong for proactive messaging where
//     every Telegram user could opt in independently.
//   - Per-tenant settings + last-sent timestamps live here.
//
// Redis schema:
//   autopilot:proactive_set         SET of tenantIds opted in
//   autopilot:last_proactive:{tid}  STRING (ISO timestamp of last sent msg)
//   autopilot:last_proactive_reason:{tid}  STRING (last decision rationale, for debug)

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";

const SET_KEY = "autopilot:proactive_set";
function lastKey(tid: string) {
  return `autopilot:last_proactive:${tid}`;
}
function lastReasonKey(tid: string) {
  return `autopilot:last_proactive_reason:${tid}`;
}

export async function enableProactive(tenantId: string): Promise<void> {
  const store = getStore();
  await store.sadd(SET_KEY, tenantId);
}

export async function disableProactive(tenantId: string): Promise<void> {
  const store = getStore();
  await store.srem(SET_KEY, tenantId);
}

export async function isProactiveEnabled(tenantId: string): Promise<boolean> {
  const store = getStore();
  const members = await store.smembers(SET_KEY);
  return members.includes(tenantId);
}

export async function listProactiveTenants(): Promise<string[]> {
  const store = getStore();
  return store.smembers(SET_KEY);
}

export async function getLastProactive(tenantId: string): Promise<number | null> {
  const store = getStore();
  const raw = await store.get<string>(lastKey(tenantId));
  if (!raw) return null;
  const n = Date.parse(raw);
  return Number.isFinite(n) ? n : null;
}

export async function getLastProactiveReason(
  tenantId: string
): Promise<string | null> {
  const store = getStore();
  return (await store.get<string>(lastReasonKey(tenantId))) ?? null;
}

export async function recordProactiveSent(
  tenantId: string,
  reason: string
): Promise<void> {
  const store = getStore();
  await store.set(lastKey(tenantId), new Date().toISOString());
  await store.set(lastReasonKey(tenantId), reason.slice(0, 300));
}

// --- heartbeat LLM short-circuit (deterministic state-change gate) -----------
//
// The proactive gate above throttles SENT messages, not LLM calls — so on an
// idle day the heartbeat would still pay for a "should I message?" prompt every
// single minute. These helpers let the runtime decide whether anything actually
// changed (a job finished, a new one started, the user messaged) before
// spending a token: we store a fingerprint of the observable state + the time
// of the last LLM decision, and the heartbeat only calls the model when the
// fingerprint moves or a long idle interval has elapsed.

function hbFpKey(tid: string) {
  return `autopilot:hb_fp:${tid}`;
}
function hbLlmKey(tid: string) {
  return `autopilot:hb_llm:${tid}`;
}

export async function getHeartbeatLlmState(
  tenantId: string
): Promise<{ fp: string | null; lastLlmMs: number | null }> {
  const store = getStore();
  const [fp, lastRaw] = await Promise.all([
    store.get<string>(hbFpKey(tenantId)),
    store.get<string | number>(hbLlmKey(tenantId)),
  ]);
  const lastLlmMs = lastRaw == null ? null : Number(lastRaw);
  return {
    fp: fp ?? null,
    lastLlmMs: Number.isFinite(lastLlmMs as number) ? (lastLlmMs as number) : null,
  };
}

export async function recordHeartbeatLlm(
  tenantId: string,
  fingerprint: string
): Promise<void> {
  const store = getStore();
  await Promise.all([
    store.set(hbFpKey(tenantId), fingerprint.slice(0, 600)),
    store.set(hbLlmKey(tenantId), String(Date.now())),
  ]);
}

// Longest the heartbeat will go WITHOUT an LLM check even when nothing
// observable changed — lets it still surface deferred reminders ("check back
// tomorrow") that live only in memory and don't move the fingerprint.
export function heartbeatMaxIdleMs(): number {
  const n = Number(env("AUTOPILOT_HEARTBEAT_MAX_IDLE_MIN") ?? "360"); // 6h
  return Number.isFinite(n) && n > 0 ? n * 60 * 1000 : 6 * 60 * 60 * 1000;
}

// --- gating helpers ---------------------------------------------------------

export function cooldownMs(): number {
  // 15 min default — long enough to feel like a real person who picks their
  // moments, short enough that the user actually sees the bot fire on a
  // typical day. Env override (AUTOPILOT_PROACTIVE_COOLDOWN_MIN) wins.
  const n = Number(env("AUTOPILOT_PROACTIVE_COOLDOWN_MIN") ?? "15");
  return Number.isFinite(n) && n > 0 ? n * 60 * 1000 : 15 * 60 * 1000;
}

// UTC quiet hours: skip heartbeat between [start, end). Hours wrap if start > end.
// Env: AUTOPILOT_QUIET_HOURS_START / _END as integer hour (0-23). Default
// 5..14 UTC = 12am..9am US Pacific, a reasonable "don't ping me while asleep".
export function isQuietHourNow(now = new Date()): boolean {
  const start = Number(env("AUTOPILOT_QUIET_HOURS_START") ?? "5");
  const end = Number(env("AUTOPILOT_QUIET_HOURS_END") ?? "14");
  if (
    !Number.isFinite(start) ||
    !Number.isFinite(end) ||
    start === end ||
    start < 0 ||
    start > 23 ||
    end < 0 ||
    end > 23
  ) {
    return false;
  }
  const h = now.getUTCHours();
  return start < end ? h >= start && h < end : h >= start || h < end;
}

// Whether the heartbeat is allowed to RUN at all for this tenant right now.
// Deterministic — same Redis state + same wall-clock returns the same answer.
export type GateDecision =
  | { allowed: true }
  | {
      allowed: false;
      reason:
        | "not_enabled"
        | "quiet_hours"
        | "cooldown_active"
        | "recently_user_active";
      detail?: string;
    };

export async function checkProactiveGate(args: {
  tenantId: string;
  lastUserMessageMs?: number | null;
}): Promise<GateDecision> {
  if (!(await isProactiveEnabled(args.tenantId))) {
    return { allowed: false, reason: "not_enabled" };
  }
  if (isQuietHourNow()) {
    return { allowed: false, reason: "quiet_hours" };
  }
  const last = await getLastProactive(args.tenantId);
  const cd = cooldownMs();
  if (last != null && Date.now() - last < cd) {
    return {
      allowed: false,
      reason: "cooldown_active",
      detail: `last sent ${Math.round((Date.now() - last) / 60000)} min ago; cooldown ${cd / 60000} min`,
    };
  }
  // Don't proactively bug them right after they messaged — give a 2-min
  // grace window so the bot doesn't feel like it's hovering. Shorter than
  // before (5 min) because the heartbeat is the main path through which
  // job-complete pings travel, and 5 min after-the-fact pings start to
  // feel late.
  const graceMs = Number(env("AUTOPILOT_USER_GRACE_MIN") ?? "2") * 60 * 1000;
  if (
    args.lastUserMessageMs != null &&
    Date.now() - args.lastUserMessageMs < graceMs
  ) {
    return { allowed: false, reason: "recently_user_active" };
  }
  return { allowed: true };
}
