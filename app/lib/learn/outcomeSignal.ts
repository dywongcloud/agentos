// app/lib/learn/outcomeSignal.ts
//
// Defines what "quality" means for an agentOS turn/job — correction/retry
// detection for chat, success/failure(+struggle) for jobs — and orchestrates
// capturing it with minimal invasiveness: reads back the per-turn attribution
// stashed during retrieval, computes a quality score, and fans it out to
// routerBias/memoryBias/relationGraph.
//
// This is also the module that closes the success-only asymmetry in the
// existing code (failJobStep never calls recordSolution): captureJobOutcome
// gives failures a real, separate, additive negative learning signal without
// touching finalizeJobStep/failJobStep's existing deterministic gating at all.

import { getStore } from "@/app/lib/store";
import { learnDisabled } from "@/app/lib/learn/stableStats";
import { recordChatRouteOutcomeByBucket, recordPendingPurposeOutcome } from "@/app/lib/learn/routerBias";
import { recordMemoryKindOutcome, recordSolutionSourceOutcome } from "@/app/lib/learn/memoryBias";
import { recordMemoryCooccurrence } from "@/app/lib/learn/relationGraph";
import type { MemoryKind } from "@/app/lib/memoryStore";
import type { SolutionSource } from "@/app/lib/solutionMemory";

export type TurnAttribution = {
  chat?: { arm: "base" | "esc"; bucket: string };
  memory?: Array<{ id: string; kind: string }>;
  solution?: Array<{ id: string; source: string }>;
};

const TTL_SECONDS = 21600;

function attrKey(tenantId: string, turnKey: string): string {
  return `learn:attr:${tenantId}:${turnKey}`;
}

// store.get() may hand back the raw JSON string we wrote OR an
// already-decoded object (Upstash auto-parses anything that looks like JSON),
// depending on backend — accept either, and treat anything else as absent.
function parseAttr(raw: unknown): TurnAttribution | null {
  try {
    const val = typeof raw === "string" ? JSON.parse(raw) : raw;
    if (val && typeof val === "object") return val as TurnAttribution;
    return null;
  } catch {
    return null;
  }
}

export async function stashTurnAttribution(
  tenantId: string,
  turnKey: string,
  patch: Partial<TurnAttribution>
): Promise<void> {
  try {
    if (learnDisabled()) return;
    const store = getStore();
    const key = attrKey(tenantId, turnKey);
    const existing = parseAttr(await store.get<string>(key)) ?? {};
    const merged: TurnAttribution = { ...existing, ...patch };
    await store.set(key, JSON.stringify(merged), { exSeconds: TTL_SECONDS });
  } catch {
    // swallow: best-effort learning signal, never correctness-bearing
  }
}

export async function readAndClearTurnAttribution(
  tenantId: string,
  turnKey: string
): Promise<TurnAttribution | null> {
  const store = getStore();
  const key = attrKey(tenantId, turnKey);
  try {
    const parsed = parseAttr(await store.get<string>(key));
    // Always clear on read (even if parsing failed) so a retry or duplicate
    // delivery can't double-consume — or re-trip on — the same attribution.
    try {
      await store.del(key);
    } catch {
      // swallow
    }
    return parsed;
  } catch {
    return null;
  }
}

const CORRECTION_LEAD =
  /^\s*(no+[,!.\s]|nope\b|wrong\b|that'?s\s+(not|wrong|incorrect)|not\s+(what|that|quite)|i\s+meant|i\s+said|try\s+again|redo\b|actually[, ]|that\s+isn'?t|incorrect\b|not\s+right\b)/i;

export function looksLikeCorrection(newUserText: string): boolean {
  const text = (newUserText ?? "").trim();
  if (CORRECTION_LEAD.test(text)) return true;
  return text.length < 24 && /\b(no|wrong|nope|bad|ugh)\b/i.test(text);
}

// priorAssistantText is accepted now for forward-compatibility (e.g. future
// echo-of-apology detection) but unused in this v1 heuristic. 0.65 is an
// honest, deliberately modest default — the app has no explicit rating UI
// today, so an uncorrected turn is treated as merely "probably fine", not great.
export function computeChatTurnQuality(newUserText: string, priorAssistantText: string): number {
  void priorAssistantText;
  return looksLikeCorrection(newUserText) ? 0.15 : 0.65;
}

export async function captureChatOutcome(
  tenantId: string,
  sessionId: string,
  newUserText: string,
  priorAssistantText: string
): Promise<void> {
  try {
    if (learnDisabled()) return;
    const attr = await readAndClearTurnAttribution(tenantId, sessionId);
    if (!attr) return;

    const quality = computeChatTurnQuality(newUserText, priorAssistantText);

    if (attr.chat) {
      await recordChatRouteOutcomeByBucket(attr.chat.bucket, attr.chat.arm, quality);
    }

    if (attr.memory?.length) {
      for (const m of attr.memory) {
        await recordMemoryKindOutcome(tenantId, m.kind as MemoryKind, quality);
      }
      if (attr.memory.length >= 2) {
        await recordMemoryCooccurrence(tenantId, attr.memory.map((m) => m.id), quality);
      }
    }

    if (attr.solution?.length) {
      for (const s of attr.solution) {
        await recordSolutionSourceOutcome(tenantId, s.source as SolutionSource, quality);
      }
    }
  } catch {
    // swallow: fire-and-forget from session.ts, must never throw
  }
}

export async function captureJobOutcome(
  tenantId: string,
  jobId: string,
  outcome: "success" | "failure",
  opts?: { struggled?: boolean }
): Promise<void> {
  try {
    if (learnDisabled()) return;
    const quality = outcome === "success" ? (opts?.struggled ? 0.75 : 0.9) : 0.1;

    // Keyed by jobId alone (orchestrateStep, where maybeUpgradePurpose is
    // called, has no tenantId in scope) — independent of the tenant-scoped
    // TurnAttribution mailbox read below, so this fires even when that
    // mailbox is empty (e.g. a job with no memory/solution retrieval at all).
    await recordPendingPurposeOutcome(jobId, quality).catch(() => {});

    const attr = await readAndClearTurnAttribution(tenantId, jobId);
    if (!attr) return;

    if (attr.memory?.length) {
      for (const m of attr.memory) {
        await recordMemoryKindOutcome(tenantId, m.kind as MemoryKind, quality);
      }
    }

    if (attr.solution?.length) {
      for (const s of attr.solution) {
        await recordSolutionSourceOutcome(tenantId, s.source as SolutionSource, quality);
      }
    }

    if (attr.memory && attr.memory.length >= 2) {
      await recordMemoryCooccurrence(tenantId, attr.memory.map((m) => m.id), quality);
    }
  } catch {
    // swallow: this is an additive negative-learning channel on top of the
    // existing deterministic failJobStep/finalizeJobStep path — it must
    // never affect it, in either direction
  }
}
