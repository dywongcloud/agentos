// app/lib/toolMiddleware.ts
//
// withActivityLogging — wraps every tool's `execute` to record one activity
// entry per call (and one per failure). Drop-in: returns a new ToolSet with
// the same shape, so agentTurn doesn't have to change its call sites.
//
// Why a wrapper (not OpenAI/AI-SDK telemetry hooks): we want per-tenant
// scoping, secret-redacted arg summaries, and the same Redis path the rest
// of the activity log uses. AI SDK telemetry is too coarse for that.

import type { ToolSet } from "ai";
import { recordActivity } from "@/app/lib/activityLog";
import { recordAudit, type AuditKind } from "@/app/lib/auditLog";
import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";

// Composio's tool router runs COMPOSIO_SEARCH_TOOLS through its own planning
// LLM, exposing an optional `model` hint to the caller. The calling agent
// tends to copy the schema's "gpt-4o" example into that field, so discovery /
// tool-search planning silently runs on gpt-4o. Force our chosen search model
// (default gpt-4.1-mini, override via COMPOSIO_SEARCH_MODEL) on that one tool —
// `model` is only accepted by the search call, not execute/multi-execute, so
// we never inject it into a tool that would reject the arg.
function normalizeComposioArgs(
  name: string,
  namespace: string | undefined,
  args: unknown
): unknown {
  if (namespace !== "composio" || name !== "COMPOSIO_SEARCH_TOOLS") return args;
  if (!args || typeof args !== "object") return args;
  const desired = env("COMPOSIO_SEARCH_MODEL") ?? "gpt-4.1-mini";
  if ((args as Record<string, unknown>).model === desired) return args;
  return { ...(args as Record<string, unknown>), model: desired };
}

// Strip OAuth/connect redirect URLs out of Composio connection-management
// results before the model sees them. When the model relays such a URL it
// often mangles it (truncated params) or substitutes a remembered generic
// app.composio.dev link — and links replayed from history are expired. The
// ONLY sanctioned path for connect links is start_integration_auth, which
// delivers the exact minted URL to the user's chat itself.
const CONNECT_RESULT_TOOLS = new Set([
  "COMPOSIO_MANAGE_CONNECTIONS",
  "COMPOSIO_INITIATE_CONNECTION",
  "COMPOSIO_CREATE_CONNECTION",
]);
const AUTH_URL_RE =
  /https?:\/\/[^\s"']*(?:composio\.dev|connect|oauth|redirect)[^\s"']*/gi;

function stripAuthUrls(v: unknown, depth = 0): unknown {
  if (depth > 6 || v == null) return v;
  if (typeof v === "string") {
    return v.replace(
      AUTH_URL_RE,
      "[auth link removed — use the start_integration_auth tool; it sends the real link to the user itself]"
    );
  }
  if (Array.isArray(v)) return v.map((x) => stripAuthUrls(x, depth + 1));
  if (typeof v === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, val] of Object.entries(v as Record<string, unknown>)) {
      out[k] = stripAuthUrls(val, depth + 1);
    }
    return out;
  }
  return v;
}

function sanitizeComposioResult(
  name: string,
  namespace: string | undefined,
  result: unknown
): unknown {
  if (namespace !== "composio" || !CONNECT_RESULT_TOOLS.has(name)) return result;
  return stripAuthUrls(result);
}

// Hard-block Composio's connection-management meta-tool. It hands back hosted
// connect.composio.dev/link/lk_* links (project-generic, often dead) that the
// model then relays instead of the correct per-tenant backend.composio.dev
// redirect from initiateConnection — the source of repeated wrong/expired
// connect links in prod. Everything it does is covered by our own tools:
// status → check_integration_connected, links → start_integration_auth (which
// delivers the exact minted URL itself). Escape hatch:
// COMPOSIO_ALLOW_MANAGE_CONNECTIONS=1.
function blockedComposioConnectionCall(
  name: string,
  namespace: string | undefined
): Record<string, unknown> | null {
  if (namespace !== "composio" || name !== "COMPOSIO_MANAGE_CONNECTIONS") return null;
  if ((env("COMPOSIO_ALLOW_MANAGE_CONNECTIONS") ?? "0") === "1") return null;
  return {
    ok: false,
    blocked: true,
    error:
      "COMPOSIO_MANAGE_CONNECTIONS is disabled in this deployment because its " +
      "hosted connect links are wrong for this tenant.",
    instruction:
      "To check whether an app is connected, call check_integration_connected. " +
      "To connect or reconnect an app, call start_integration_auth — it mints " +
      "the correct link and sends it to the user itself. NEVER write a connect " +
      "URL in your reply.",
  };
}

// Classify a tool call into an audit kind, or null if it shouldn't go in the
// audit log. Only state-modifying / live-action tools surface to audit so the
// stream stays signal-heavy — read tools (read_virtual_file,
// list_session_assets, COMPOSIO_SEARCH_TOOLS, etc.) stay activity-only.
function auditKindForTool(
  name: string,
  namespace: string | undefined,
  args: unknown
): { kind: AuditKind; summary: string; meta: Record<string, unknown> } | null {
  // Composio executor → audit (real external mutation potential).
  if (
    namespace === "composio" &&
    (name === "COMPOSIO_EXECUTE_TOOL" || name === "COMPOSIO_MULTI_EXECUTE_TOOL")
  ) {
    const a = (args ?? {}) as Record<string, unknown>;
    const slug =
      (a.tool_slug as string | undefined) ??
      (a.toolSlug as string | undefined) ??
      (a.tool as string | undefined) ??
      "(unknown)";
    return {
      kind: "tool.composio_exec",
      summary: `composio: ${slug}`,
      meta: { name, namespace, slug },
    };
  }

  // VFS write — a concrete file just changed on the per-tenant VFS.
  if (name === "write_virtual_file") {
    const a = (args ?? {}) as Record<string, unknown>;
    return {
      kind: "tool.vfs_write",
      summary: `vfs write: ${a.path ?? "(unknown path)"}`,
      meta: { name, path: a.path },
    };
  }

  // virtual_shell — capable of rm / mv / mkdir / cp, audit-worthy.
  if (name === "virtual_shell") {
    const a = (args ?? {}) as Record<string, unknown>;
    const cmd = String(a.command ?? a.cmd ?? "");
    return {
      kind: "tool.vfs_shell",
      summary: `vfs shell: ${cmd.slice(0, 140)}`,
      meta: { name, command: cmd.slice(0, 400) },
    };
  }

  // Browser actions — they touch the live web on the user's behalf.
  if (name === "browse_web" || name === "login_to_site") {
    const a = (args ?? {}) as Record<string, unknown>;
    const goal = String(a.goal ?? a.start_url ?? a.host ?? "");
    return {
      kind: "tool.browser",
      summary: `browser ${name === "login_to_site" ? "login" : "browse"}: ${goal.slice(0, 140)}`,
      meta: { name, goal: goal.slice(0, 400) },
    };
  }

  return null;
}

const REDACT_KEY_RE = /pass(?:word)?|secret|token|api[_-]?key|bearer|auth/i;

function shortVal(v: unknown): string {
  if (v == null) return String(v);
  if (typeof v === "string") {
    return JSON.stringify(v.length > 60 ? v.slice(0, 60) + "…" : v);
  }
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  if (Array.isArray(v)) return `[${v.length}]`;
  if (typeof v === "object") return "{…}";
  return String(v);
}

function summarizeArgs(args: unknown): string {
  if (!args || typeof args !== "object") return "";
  const entries = Object.entries(args as Record<string, unknown>).slice(0, 5);
  return entries
    .map(([k, v]) => {
      if (REDACT_KEY_RE.test(k)) return `${k}=[REDACTED]`;
      return `${k}=${shortVal(v)}`;
    })
    .join(", ");
}

function redactArgs(args: unknown): unknown {
  if (!args || typeof args !== "object") return args;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(args as Record<string, unknown>)) {
    if (REDACT_KEY_RE.test(k)) {
      out[k] = "[REDACTED]";
    } else if (typeof v === "string" && v.length > 200) {
      out[k] = v.slice(0, 200) + "…";
    } else {
      out[k] = v;
    }
  }
  return out;
}

// --- per-tool timeout ------------------------------------------------------
//
// A single hung tool call (a stuck headless browser, a rate-limited Composio
// EXECUTE that never returns, a wedged sandbox command) would otherwise burn
// the ENTIRE per-turn deadline (8 min) with zero time left for the model to
// recover. This wrapper races each tool's execute against a generous
// per-category cap and, on timeout, returns a structured error the model can
// read and route around — instead of stalling the whole turn.
//
// Caps are deliberately generous: they exist to catch genuine hangs, not to
// cut off legitimately-slow work. Long-running tools (browser, coding agent)
// get a higher cap. All overridable via env; set TOOL_TIMEOUT_MS_DEFAULT=0 to
// disable entirely.
//
// Note: a timed-out call is abandoned, not cancelled — the underlying request
// may still complete in the background. For mutating tools the cap is high
// enough that it only fires on a true hang, where a late-completing side
// effect is preferable to a dead turn.

// Tools that legitimately run long; given a roomier cap.
const LONG_RUNNING_TOOLS = new Set([
  "browse_web",
  "login_to_site",
  "ask_claude_code",
  "ask_gpt5",
  "publish_vfs_to_github",
  "summarize_chat_and_remember",
]);

function intFromEnv(name: string, fallback: number): number {
  const raw = env(name);
  if (raw == null || raw === "") return fallback;
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? n : fallback;
}

function timeoutMsForTool(name: string, namespace?: string): number {
  const def = intFromEnv("TOOL_TIMEOUT_MS_DEFAULT", 90_000);
  if (def === 0) return 0; // disabled
  if (LONG_RUNNING_TOOLS.has(name)) {
    return intFromEnv("TOOL_TIMEOUT_MS_LONG", 300_000); // 5 min
  }
  if (namespace === "composio") {
    return intFromEnv("TOOL_TIMEOUT_MS_COMPOSIO", 120_000); // 2 min
  }
  return def;
}

export function withToolTimeout<T extends ToolSet>(
  tools: T,
  opts: { namespace?: string } = {}
): T {
  const out: Record<string, unknown> = {};
  for (const [name, def] of Object.entries(tools)) {
    const d = def as ToolDef | undefined;
    if (!d || typeof d !== "object" || typeof d.execute !== "function") {
      out[name] = def;
      continue;
    }
    const capMs = timeoutMsForTool(name, opts.namespace);
    if (capMs <= 0) {
      out[name] = def;
      continue;
    }
    const originalExecute = d.execute;
    out[name] = {
      ...d,
      execute: async (args: unknown, ctx?: unknown) => {
        let timer: ReturnType<typeof setTimeout> | undefined;
        const timeout = new Promise<{ ok: false; error: string; timedOut: true }>(
          (resolve) => {
            timer = setTimeout(
              () =>
                resolve({
                  ok: false,
                  timedOut: true,
                  error:
                    `tool '${name}' timed out after ${Math.round(capMs / 1000)}s and was skipped. ` +
                    `Do not retry it identically — try a different tool, simpler input, or tell the user what blocked you.`,
                }),
              capMs
            );
          }
        );
        try {
          return await Promise.race([
            Promise.resolve(originalExecute(args, ctx)),
            timeout,
          ]);
        } finally {
          if (timer) clearTimeout(timer);
        }
      },
    };
  }
  return out as T;
}

// --- idempotency guard ------------------------------------------------------
//
// Deep jobs and automations run unwatched, and WDK retries a failed
// executeAgentTurnStep (and the agent itself may re-issue a tool after a
// transient error). Without a guard, a retried turn can re-run an EXTERNAL
// mutation — most painfully, send the same email/message twice. This wrapper
// gives the two Composio executors at-most-once semantics within a single
// job: the first successful call's result is cached under a hash of its args,
// and an identical later call returns that cached result (flagged) instead of
// firing the side effect again.
//
// Scope rules (deliberate):
//   - Keyed by tenant + scopeId (the JOB/automation run id) + tool + args hash.
//     Different jobs with the same args are NOT deduped.
//   - Only engaged when scopeId is set. Interactive chat passes none, because a
//     chat session id spans many independent user requests — caching there
//     would wrongly swallow a legitimate "send that again".
//   - Only SUCCESSES are cached; a failed/timed-out/ok:false call stays freely
//     retryable (that's the whole point of retrying).
const IDEMPOTENT_TOOLS = new Set([
  "COMPOSIO_EXECUTE_TOOL",
  "COMPOSIO_MULTI_EXECUTE_TOOL",
]);

// Stable, key-sorted JSON so {a,b} and {b,a} hash identically.
function stableStringify(v: unknown): string {
  if (v === null || typeof v !== "object") return JSON.stringify(v) ?? "null";
  if (Array.isArray(v)) return "[" + v.map(stableStringify).join(",") + "]";
  const keys = Object.keys(v as Record<string, unknown>).sort();
  return (
    "{" +
    keys
      .map(
        (k) =>
          JSON.stringify(k) +
          ":" +
          stableStringify((v as Record<string, unknown>)[k])
      )
      .join(",") +
    "}"
  );
}

// djb2 — deterministic, no node:crypto (keeps this module workflow-VM safe).
function hashStr(s: string): string {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  return (h >>> 0).toString(36);
}

export type WithIdempotencyOptions = {
  namespace?: string;
  scopeId?: string;
  ttlSeconds?: number;
};

export function withIdempotency<T extends ToolSet>(
  tools: T,
  tenantId: string,
  opts: WithIdempotencyOptions = {}
): T {
  if (!opts.scopeId) return tools;
  const store = getStore();
  const ttl = opts.ttlSeconds ?? 1800;
  const out: Record<string, unknown> = {};
  for (const [name, def] of Object.entries(tools)) {
    const d = def as ToolDef | undefined;
    if (
      !d ||
      typeof d !== "object" ||
      typeof d.execute !== "function" ||
      !IDEMPOTENT_TOOLS.has(name)
    ) {
      out[name] = def;
      continue;
    }
    const originalExecute = d.execute;
    out[name] = {
      ...d,
      execute: async (args: unknown, ctx?: unknown) => {
        const key = `idem:${tenantId}:${opts.scopeId}:${name}:${hashStr(
          stableStringify(args)
        )}`;
        try {
          const prior = await store.get<{ result: unknown }>(key);
          if (prior && typeof prior === "object" && "result" in prior) {
            const note =
              "This exact call already executed successfully earlier in this job; " +
              "returning the prior result instead of re-running it, to avoid a " +
              "duplicate external action (e.g. a double-sent email). If you " +
              "genuinely need to repeat the action, change a parameter.";
            const base = prior.result;
            if (base && typeof base === "object" && !Array.isArray(base)) {
              return {
                ...(base as Record<string, unknown>),
                idempotent_replay: true,
                note,
              };
            }
            return { result: base, idempotent_replay: true, note };
          }
        } catch {
          // A cache-read failure must never block the real call.
        }
        const result = await originalExecute(args, ctx);
        const failed =
          result != null &&
          typeof result === "object" &&
          ((result as Record<string, unknown>).ok === false ||
            (result as Record<string, unknown>).error != null);
        if (!failed) {
          try {
            await store.set(key, { result }, { exSeconds: ttl });
          } catch {
            // Best-effort cache; a write failure just means no dedupe.
          }
        }
        return result;
      },
    };
  }
  return out as T;
}

export type WithActivityLoggingOptions = {
  // Don't wrap these tool names — useful if a tool already logs its own
  // activity entries and we'd otherwise double-count.
  skipNames?: string[];
  // Optional name prefix surfaced in the activity log entry. Lets us tell
  // "composio:tool" apart from "native:tool" later.
  namespace?: string;
};

type ToolDef = { execute?: (args: unknown, ctx?: unknown) => unknown } & Record<
  string,
  unknown
>;

export function withActivityLogging<T extends ToolSet>(
  tools: T,
  tenantId: string,
  opts: WithActivityLoggingOptions = {}
): T {
  const out: Record<string, unknown> = {};
  const skip = new Set(opts.skipNames ?? []);
  const tag = opts.namespace ? `${opts.namespace}:` : "";

  for (const [name, def] of Object.entries(tools)) {
    const d = def as ToolDef | undefined;
    if (
      !d ||
      typeof d !== "object" ||
      typeof d.execute !== "function" ||
      skip.has(name)
    ) {
      out[name] = def;
      continue;
    }

    const originalExecute = d.execute;
    out[name] = {
      ...d,
      execute: async (rawArgs: unknown, ctx?: unknown) => {
        const args = normalizeComposioArgs(name, opts.namespace, rawArgs);
        const argSummary = summarizeArgs(args);
        const startedAt = Date.now();
        await recordActivity(tenantId, {
          kind: "tool",
          summary: `${tag}${name}(${argSummary})`,
          meta: { tool: name, namespace: opts.namespace, args: redactArgs(args) },
        });
        // Connection management must go through our own tools (see
        // blockedComposioConnectionCall) — short-circuit with an instruction.
        const blocked = blockedComposioConnectionCall(name, opts.namespace);
        if (blocked) return blocked;
        try {
          const result = await originalExecute(args, ctx);
          const elapsedMs = Date.now() - startedAt;
          // Trailing entry only on slowish calls — keeps the log readable
          // when fast tools fire in bursts.
          if (elapsedMs > 1500) {
            await recordActivity(tenantId, {
              kind: "tool",
              summary: `${tag}${name} ✓ ${(elapsedMs / 1000).toFixed(1)}s`,
              meta: { tool: name, ok: true, elapsedMs },
            });
          }
          // Mirror high-impact tool calls to the AUDIT log so dashboard
          // operators can see them alongside integration / settings
          // changes. Read-only tools return null from auditKindForTool
          // and are activity-only.
          const audit = auditKindForTool(name, opts.namespace, args);
          if (audit) {
            await recordAudit(tenantId, {
              kind: audit.kind,
              summary: audit.summary,
              after: "ok",
              meta: { ...audit.meta, elapsedMs },
            });
          }
          return sanitizeComposioResult(name, opts.namespace, result);
        } catch (err: unknown) {
          const msg = err instanceof Error ? err.message : String(err);
          await recordActivity(tenantId, {
            kind: "tool",
            summary: `${tag}${name} ✗ ${msg.slice(0, 140)}`,
            meta: { tool: name, ok: false, error: msg.slice(0, 400) },
          });
          // Surface failed audit-worthy actions too so a broken Composio
          // call or VFS write isn't invisible.
          const audit = auditKindForTool(name, opts.namespace, args);
          if (audit) {
            await recordAudit(tenantId, {
              kind: audit.kind,
              summary: `${audit.summary} ✗ ${msg.slice(0, 100)}`,
              after: "error",
              meta: { ...audit.meta, error: msg.slice(0, 300) },
            });
          }
          throw err;
        }
      },
    };
  }
  return out as T;
}
