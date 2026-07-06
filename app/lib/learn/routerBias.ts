// app/lib/learn/routerBias.ts
//
// FastGRNN-spirit adaptive routing bias: per (decision, context-bucket)
// running quality stats used to NUDGE a choice between two ALREADY
// deterministically-valid options. This module never invents a third
// option and never bypasses an existing env/key floor (e.g. fableEnabled()) —
// it can only pick which of two pre-approved candidates to return.

import { getStore } from "@/app/lib/store";
import { confidence, updateStat, type StatRecord, DEFAULT_STAT } from "@/app/lib/learn/stableStats";
import { learnDisabled } from "@/app/lib/learn/stableStats";
import {
  chatModelNameFor,
  chatModelName,
  agenticChatModelName,
  fableEnabled,
  looksComplexAgentic,
} from "@/app/lib/modelRouting";

const MIN_CONF = 0.5;
const MARGIN = 0.12;

export function contextBucketFor(text: string): string {
  const len = (text ?? "").length;
  const lenTier = len < 60 ? "s" : len < 200 ? "m" : "l";
  const complexTier = looksComplexAgentic(text) ? "c1" : "c0";
  return `${lenTier}:${complexTier}`;
}

export async function recordChatRouteOutcomeByBucket(
  bucket: string,
  arm: "base" | "esc",
  quality: number
): Promise<void> {
  try {
    const store = getStore();
    const key = `learn:route:chat:${arm}:${bucket}`;
    await updateStat(store, key, "stat", quality);
  } catch {
    // swallow: recording failures must never surface to callers
  }
}

export type ChatRouteDecision = { model: string; arm: "base" | "esc"; bucket: string };

// Returns the model to use PLUS which arm/bucket that decision landed in, so
// the caller can stash it (see app/workflows/session.ts) and score it once the
// user's next message arrives — closing the loop that the read side alone
// cannot close. armFor mirrors the two candidate pools this function chooses
// between: "esc" whenever the returned model equals agenticChatModelName().
function armFor(model: string, escalated: string): "base" | "esc" {
  return model === escalated ? "esc" : "base";
}

export async function chatModelNameForLearned(text: string): Promise<ChatRouteDecision> {
  const bucket = contextBucketFor(text);
  // chatModelNameFor is pre-existing, pure/non-throwing code, but it's called
  // here from a NEW code path (this function) that upstream callers now run
  // concurrently with unrelated work (see app/workflows/session.ts) — so an
  // unexpected throw here must degrade to the deterministic model choice
  // instead of rejecting this whole function and taking neighboring
  // Promise.all work down with it.
  let deterministic: string;
  try {
    deterministic = chatModelNameFor(text);
  } catch {
    const model = chatModelName();
    return { model, arm: armFor(model, agenticChatModelNameSafe()), bucket };
  }
  try {
    if (learnDisabled()) return { model: deterministic, arm: armFor(deterministic, agenticChatModelName()), bucket };
    if (!fableEnabled()) return { model: deterministic, arm: armFor(deterministic, agenticChatModelName()), bucket };

    const base = chatModelName();
    const escalated = agenticChatModelName();
    if (base === escalated) return { model: deterministic, arm: "base", bucket };

    const store = getStore();
    const baseKey = `learn:route:chat:base:${bucket}`;
    const escKey = `learn:route:chat:esc:${bucket}`;
    const [baseRaw, escRaw] = await store.pipelineMany([
      ["HGET", baseKey, "stat"],
      ["HGET", escKey, "stat"],
    ]);

    const baseStat = parseStat(baseRaw);
    const escStat = parseStat(escRaw);

    const now = Date.now();
    const confBase = confidence(baseStat, now);
    const confEsc = confidence(escStat, now);
    if (confBase < MIN_CONF || confEsc < MIN_CONF) {
      return { model: deterministic, arm: armFor(deterministic, escalated), bucket };
    }

    const mine = deterministic === escalated ? escStat : baseStat;
    const other = deterministic === escalated ? baseStat : escStat;
    if (other.mean - mine.mean >= MARGIN) {
      const model = deterministic === escalated ? base : escalated;
      return { model, arm: armFor(model, escalated), bucket };
    }
    return { model: deterministic, arm: armFor(deterministic, escalated), bucket };
  } catch {
    // deterministic was already computed successfully above (that call is
    // guarded separately), so reuse it here rather than re-invoking
    // chatModelNameFor and risking a second, redundant failure surface.
    return { model: deterministic, arm: armFor(deterministic, agenticChatModelNameSafe()), bucket };
  }
}

// agenticChatModelName can itself throw in principle (env resolution) — the
// catch-all branches above must not risk a SECOND throw while computing the
// fallback arm label, so this collapses any failure to "base" (an arm label
// is advisory telemetry, never worth a crash).
function agenticChatModelNameSafe(): string {
  try {
    return agenticChatModelName();
  } catch {
    return "";
  }
}

const PURPOSE_PENDING_TTL_SECONDS = 21600;

function purposePendingKey(jobId: string): string {
  return `learn:route:purpose:pending:${jobId}`;
}

// maybeUpgradePurpose is called once per orchestrator iteration within a
// single job — jobId, when passed, stashes THIS iteration's decision so
// captureJobOutcome (called once, when the job finalizes/fails) can score it.
// Later iterations simply overwrite the pending key; only the last
// iteration's purpose decision before finalize is scored, which is the
// correct target (it's the decision that most shaped the delivered answer).
export async function maybeUpgradePurpose<P extends string>(
  basePurpose: P,
  upgradedPurpose: P,
  decisionKey: string,
  bucket: string,
  jobId?: string
): Promise<P> {
  let result: P = basePurpose;
  try {
    if (learnDisabled()) return basePurpose;
    if (basePurpose === upgradedPurpose) return basePurpose;

    const store = getStore();
    const baseKey = `learn:route:purpose:${decisionKey}:base:${bucket}`;
    const escKey = `learn:route:purpose:${decisionKey}:esc:${bucket}`;
    const [baseRaw, escRaw] = await store.pipelineMany([
      ["HGET", baseKey, "stat"],
      ["HGET", escKey, "stat"],
    ]);

    const baseStat = parseStat(baseRaw);
    const escStat = parseStat(escRaw);

    const now = Date.now();
    const confBase = confidence(baseStat, now);
    const confEsc = confidence(escStat, now);
    if (confBase >= MIN_CONF && confEsc >= MIN_CONF && escStat.mean - baseStat.mean >= MARGIN) {
      result = upgradedPurpose;
    }
    return result;
  } catch {
    return basePurpose;
  } finally {
    if (jobId) {
      const arm = result === upgradedPurpose ? "esc" : "base";
      try {
        const store = getStore();
        await store.set(purposePendingKey(jobId), JSON.stringify({ decisionKey, bucket, arm }), {
          exSeconds: PURPOSE_PENDING_TTL_SECONDS,
        });
      } catch {
        // swallow: pending-decision stash is best-effort telemetry
      }
    }
  }
}

// Reads back the last-stashed maybeUpgradePurpose decision for a job (if any)
// and scores it. Called from captureJobOutcome once the job's final quality
// is known — decoupled from the per-tenant TurnAttribution mailbox because
// orchestrateStep only has jobId in scope, not tenantId.
export async function recordPendingPurposeOutcome(jobId: string, quality: number): Promise<void> {
  try {
    const store = getStore();
    const key = purposePendingKey(jobId);
    const raw = await store.get<string>(key);
    if (!raw) return;
    await store.del(key).catch(() => {});
    const parsed = typeof raw === "string" ? JSON.parse(raw) : raw;
    if (!parsed || typeof parsed.decisionKey !== "string" || typeof parsed.bucket !== "string") return;
    const arm: "base" | "esc" = parsed.arm === "esc" ? "esc" : "base";
    await updateStat(store, `learn:route:purpose:${parsed.decisionKey}:${arm}:${parsed.bucket}`, "stat", quality);
  } catch {
    // swallow
  }
}

export async function chimeHintFor(bucket: string): Promise<string> {
  try {
    if (learnDisabled()) return "";
    const store = getStore();
    const key = `learn:route:chime:${bucket}`;
    const stat = await store.hget<string>(key, "stat");
    const rec = parseStat(stat);
    const now = Date.now();
    if (confidence(rec, now) < MIN_CONF) return "";
    return rec.mean >= 0.5 ? "chime" : "quiet";
  } catch {
    return "";
  }
}

export async function recordChimeOutcome(bucket: string, wasGood: boolean): Promise<void> {
  try {
    const store = getStore();
    const key = `learn:route:chime:${bucket}`;
    await updateStat(store, key, "stat", wasGood ? 1 : 0);
  } catch {
    // swallow: recording failures must never surface to callers
  }
}

// pipelineMany decodes hash-field JSON strings back into their parsed shape
// (or leaves non-JSON-looking strings as raw strings) depending on the store
// implementation, so a HGET result may already be an object or still a raw
// JSON string — accept either and fall back to DEFAULT_STAT on anything else.
function parseStat(raw: unknown): StatRecord {
  try {
    const val = typeof raw === "string" ? JSON.parse(raw) : raw;
    if (
      val &&
      typeof (val as StatRecord).mean === "number" &&
      typeof (val as StatRecord).n === "number" &&
      typeof (val as StatRecord).updatedAt === "number"
    ) {
      return val as StatRecord;
    }
    return DEFAULT_STAT;
  } catch {
    return DEFAULT_STAT;
  }
}
