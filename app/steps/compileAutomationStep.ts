// app/steps/compileAutomationStep.ts
//
// Natural-language → structured Automation compiler. The user types
// `/automate <free-form description>`; this step asks an LLM to emit a strict
// {name, trigger, action} object that the /automate handler then materializes
// into a full Automation (minting the webhook secret, computing the next
// schedule time, registering the Composio trigger, etc.).
//
// We deliberately emit a FLAT schema (a triggerKind discriminator + nullable
// per-kind fields) rather than a zod discriminated union — OpenAI structured
// output is far more reliable with a flat shape, and we narrow it back into the
// real union in code below.
//
// Model: `meta` (gpt-5.4) by default, `meta-pro` (gpt-5.3-codex) on retry —
// never gpt-5.2. This is a meta/planning task, so it follows the same routing
// rules as the orchestrator.

import { generateObject } from "ai";
import { z } from "zod/v4";

import { buildLlmArgs } from "@/app/lib/modelRouting";
import { listTriggerTypes } from "@/app/lib/composioConnections";
import { listCustomTriggerTypes } from "@/app/lib/customTriggers";
import { isDangerousRegex } from "@/app/lib/safeRegex";

function compilesOk(pattern: string, flags: string): boolean {
  try {
    new RegExp(pattern, flags);
    return true;
  } catch {
    return false;
  }
}
import type {
  AutomationAction,
  LightStep,
  PlanStep,
  PlanValue,
} from "@/app/lib/automations";

// The compiler's output: a trigger/action SPEC without the runtime-only fields
// (schedule.nextAt, webhook.secret, composio.triggerId). The handler fills
// those in at registration time.
export type CompiledTrigger =
  | { kind: "schedule"; cron?: string; everyMs?: number; tz?: string }
  | { kind: "composio"; triggerType: string; filter?: Record<string, string> }
  | { kind: "webhook" }
  | { kind: "chat"; pattern: string; flags?: string };

export type CompiledAutomation = {
  name: string;
  trigger: CompiledTrigger;
  action: AutomationAction;
  summary: string; // one-line human-readable description for the chat reply
  // Trigger subscription config extracted from the request (e.g. { board_id }
  // for a monday polling trigger, { owner, repo } for GitHub). Passed straight
  // to registerAutomation so custom/native triggers that need config subscribe
  // in one shot instead of erroring on a missing required key.
  triggerConfig?: Record<string, string>;
};

const KNOWN_SKILLS = [
  "routing",
  "composio",
  "ssh",
  "scheduling",
  "filesystem",
  "modalities",
] as const;

const lightStepSchema = z.object({
  op: z.enum(["send", "vfs_write", "vfs_append"]),
  text: z.string().nullable(),
  path: z.string().nullable(),
  content: z.string().nullable(),
});

// A plan parameter value: exactly one of const/event/ai is meaningful per the
// `from` discriminator (the other fields are null). Flat shape for reliable
// OpenAI structured output.
const planValueSchema = z.object({
  from: z.enum(["const", "event", "ai"]),
  value: z.string().nullable(), // from === "const"
  path: z.string().nullable(), // from === "event" — dot-path into the event
  prompt: z.string().nullable(), // from === "ai" — what to generate at run time
});

// A deterministic plan step. Composio args are a list of {key,value} pairs (not
// a record) — OpenAI strict mode rejects the free-form map z.record generates.
const planStepSchema = z.object({
  op: z.enum(["send", "vfs_write", "vfs_append", "composio"]),
  // send / vfs_write / vfs_append
  text: planValueSchema.nullable(),
  path: z.string().nullable(),
  content: planValueSchema.nullable(),
  // composio
  tool: z.string().nullable(),
  args: z
    .array(z.object({ key: z.string(), value: planValueSchema }))
    .nullable(),
  connectedAccountId: z.string().nullable(),
});

const compileSchema = z.object({
  name: z.string(),
  summary: z.string(),

  triggerKind: z.enum(["schedule", "composio", "webhook", "chat"]),

  // schedule — exactly one of cron / everyMs
  cron: z.string().nullable(),
  everyMs: z.number().nullable(),
  tz: z.string().nullable(),

  // composio — the exact trigger slug is resolved dynamically from the LIVE
  // catalog (see resolveComposioTriggerSlug), NOT chosen from a hardcoded list.
  // The model supplies a toolkit + a short event query to drive that lookup,
  // plus an optional best-guess slug used only as a fallback.
  composioToolkit: z.string().nullable(), // app/toolkit slug, e.g. the service name lowercased
  composioQuery: z.string().nullable(), // short phrase describing the event to watch
  composioTriggerType: z.string().nullable(), // optional best-guess slug (fallback only)
  // Subscription config the trigger needs to target a specific resource,
  // extracted from the request as a JSON OBJECT STRING (same string-encoding
  // reason as composioFilter). e.g. '{"board_id":"12345"}', '{"owner":"acme",
  // "repo":"web"}', '{"channel":"#alerts"}'. Null when no config is needed.
  triggerConfig: z.string().nullable(),
  // filter is a JSON object STRING (e.g. '{"from":"a@b.com"}'), not a record:
  // OpenAI strict structured output rejects the `propertyNames` key that a
  // free-form map (z.record) generates.
  composioFilter: z.string().nullable(),

  // chat
  chatPattern: z.string().nullable(),
  chatFlags: z.string().nullable(),

  // action
  actionMode: z.enum(["job", "light", "plan"]),
  instruction: z.string().nullable(),
  deep: z.boolean().nullable(),
  skills: z.array(z.string()).nullable(),
  lightSteps: z.array(lightStepSchema).nullable(),
  planSteps: z.array(planStepSchema).nullable(),
});

function buildSystem(): string {
  return [
    "You compile a user's natural-language automation request into a strict",
    "structured rule of the form { trigger, action }. The system will run the",
    "action as a durable, fault-tolerant workflow whenever the trigger fires.",
    "",
    "Pick exactly ONE triggerKind:",
    "",
    "  schedule  — time-based. Set `cron` (standard 5-field 'min hour dom",
    "              month dow') for calendar cadences ('every weekday at 9am' →",
    "              '0 9 * * 1-5'), OR `everyMs` for simple fixed intervals",
    "              ('every 10 minutes' → 600000). Set `tz` to an IANA zone",
    "              (e.g. 'America/New_York') when the user implies local time;",
    "              otherwise leave tz null (UTC). Never set both cron and everyMs.",
    "  composio  — an external app event (works for ANY connected app). Do NOT",
    "              pick from a fixed list: set `composioToolkit` to the app's",
    "              toolkit slug (usually the service name lowercased) and",
    "              `composioQuery` to a short phrase describing the event to watch",
    "              ('new email', 'pull request opened', 'row added', 'item status",
    "              changed'). The system resolves the exact trigger slug from the",
    "              live catalog — including apps with no native events (e.g.",
    "              monday.com), which are covered by polling triggers. If you",
    "              happen to know the exact trigger slug, also put it in",
    "              `composioTriggerType` as a fallback; otherwise leave it null.",
    "              Put narrowing constraints in `composioFilter` as a JSON OBJECT",
    "              STRING of substrings that must appear in the event payload",
    "              (e.g. '{\"from\":\"alice@acme.com\"}' or '{\"subject\":\"invoice\"}').",
    "              Leave composioFilter null for no filter. It must be a string.",
    "              If the trigger must target a specific resource, extract that",
    "              config into `triggerConfig` (a JSON OBJECT STRING) using the",
    "              app's own field names as the user gave them — e.g. a board id,",
    "              a repo, a channel. Leave triggerConfig null when none is needed.",
    "  webhook   — fires when an external system POSTs to a minted URL. Use",
    "              when the user says 'when something calls/POSTs to a webhook'.",
    "  chat      — fires when an inbound chat message matches a regex. Set",
    "              `chatPattern` (a JS regex source string, NOT wrapped in",
    "              slashes) and optional `chatFlags` (default 'i').",
    "",
    "Then pick the action. Prefer a DETERMINISTIC action ('light' or 'plan')",
    "whenever the work is fully determined once compiled — most scheduled and",
    "event-triggered automations are. Deterministic actions run as durable",
    "workflows that execute the fixed steps directly and spend model tokens only",
    "on fields that are genuinely generative. Reserve 'job' for open-ended work.",
    "",
    "  actionMode 'light' — ONLY for trivially simple, pure send / virtual-file",
    "    ops with FIXED text and no external calls. Provide ordered `lightSteps`,",
    "    each one of:",
    "      { op:'send', text } — send a fixed message to the user.",
    "      { op:'vfs_write', path, content } — overwrite a virtual file.",
    "      { op:'vfs_append', path, content } — append to a virtual file.",
    "    Leave unused fields of each step null.",
    "  actionMode 'plan' — a DETERMINISTIC tool-call workflow: a fixed ordered",
    "    list of `planSteps` whose tool and parameters are known now. Use it when",
    "    the automation always does the same concrete thing — e.g. 'every morning",
    "    email me the weather', 'when a PR opens, post it to Slack'. Each step:",
    "      { op:'send', text } — send a message to the user.",
    "      { op:'vfs_write' | 'vfs_append', path, content } — write/append a file.",
    "      { op:'composio', tool, args } — call a Composio tool. `tool` is the",
    "        exact tool slug for whatever integration the task needs; `args` is a list of",
    "        {key,value} pairs whose keys are that tool's own",
    "        argument names. Only emit a composio step when you know the correct",
    "        slug and its argument names AND the args are scalar strings — if the",
    "        tool needs structured/array args or you're unsure, use 'job' instead.",
    "    Every `text`/`content`/arg `value` is a typed value object with `from`:",
    "      from:'const' → set `value` to the fixed literal.",
    "      from:'event' → set `path` to a dot-path into the trigger event payload",
    "        (e.g. 'subject', 'from', 'payload.title') to copy a field through.",
    "      from:'ai'    → set `prompt` describing what to GENERATE at run time",
    "        (e.g. 'Write a friendly 3-sentence summary of the item below.').",
    "        This is the ONLY thing that costs tokens — use it just for genuinely",
    "        generative fields, not for fixed or copied values.",
    "    Set the two unused value fields to null. Leave unused step fields null.",
    "  actionMode 'job' — for open-ended work that needs an agent to decide what",
    "    to do at run time (research, multi-step reasoning, unknown tools, or",
    "    structured API args). Provide a clear `instruction` written as a",
    "    directive to an autonomous agent that will receive the trigger's event",
    "    payload. Set `deep` true when the work is genuinely multi-step /",
    "    high-stakes and should use the deep orchestration engine; false for a",
    "    single focused agent turn. Optionally list `skills` from:",
    `      ${KNOWN_SKILLS.join(", ")}.`,
    "",
    "Always set a short human `name` (e.g. 'Summarize emails from Alice') and a",
    "one-sentence `summary` describing what the automation does.",
    "Set every field you are not using to null. Prefer a deterministic action",
    "('plan'/'light') when the steps are fixed; fall back to 'job' when the task",
    "is open-ended or you are unsure which tool/arguments it needs.",
  ].join("\n");
}

// Parse the model's composioFilter JSON-object string into a flat
// string->string map. Tolerates a missing/blank/malformed value (→ no filter)
// and coerces non-string values to strings.
function parseFilter(raw: string | null): Record<string, string> | undefined {
  if (!raw || !raw.trim()) return undefined;
  try {
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return undefined;
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (v == null) continue;
      out[k] = String(v);
    }
    return Object.keys(out).length ? out : undefined;
  } catch {
    return undefined;
  }
}

// Reconcile the model-extracted trigger config against the slug's REQUIRED
// keys. Structured output often names a key loosely (board vs board_id, boardId
// vs board_id); for each required key that's missing, adopt a provided key that
// matches once punctuation/case are stripped, renaming it to the exact required
// key. Non-required keys are preserved as-is.
function normalizeConfigKeys(
  provided: Record<string, string>,
  requiredKeys: string[]
): Record<string, string> {
  const out: Record<string, string> = { ...provided };
  const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
  for (const req of requiredKeys) {
    if (out[req] != null && out[req] !== "") continue;
    const hit = Object.keys(provided).find((k) => norm(k) === norm(req));
    if (hit && provided[hit] != null && provided[hit] !== "") {
      out[req] = provided[hit];
      if (hit !== req) delete out[hit];
    }
  }
  return out;
}

// Shared with compileWorkforceStep — the workforce compiler emits the same
// flat trigger fields and narrows them identically.
export type FlatTriggerFields = {
  triggerKind: "schedule" | "composio" | "webhook" | "chat";
  cron: string | null;
  everyMs: number | null;
  tz: string | null;
  // composioTriggerType is expected to already hold the dynamically-resolved
  // slug by the time narrowTrigger runs (see resolveComposioTriggerSlug).
  composioTriggerType: string | null;
  composioToolkit?: string | null;
  composioQuery?: string | null;
  composioFilter: string | null;
  chatPattern: string | null;
  chatFlags: string | null;
};

// The resolved trigger: the concrete slug plus the config keys that slug
// REQUIRES to subscribe (so the caller can extract/verify them). requiredConfig
// is empty when the slug needs no config or discovery was unavailable.
export type ResolvedTrigger = { slug: string; requiredConfig: string[] };

// Pull the `required` key list out of a config schema of unknown shape
// (native Composio and our custom types both use a JSON-schema-ish object).
function requiredKeysOf(schema: unknown): string[] {
  const req = (schema as { required?: unknown } | null)?.required;
  return Array.isArray(req) ? req.filter((k): k is string => typeof k === "string") : [];
}

// Resolve the concrete Composio trigger slug DYNAMICALLY from the live catalog
// instead of a hardcoded list. Merges native Composio trigger types with our
// custom polling types (monday.com built-ins + agent-registered ones), ranks by
// the model's toolkit + event query, and returns the best slug + its required
// config keys. Falls back to the model's own best-guess slug when discovery is
// unavailable or empty.
export async function resolveComposioTriggerSlug(args: {
  toolkit?: string | null;
  query?: string | null;
  guess?: string | null;
}): Promise<ResolvedTrigger> {
  const toolkit = args.toolkit?.trim() || undefined;
  const query = args.query?.trim() || undefined;
  const guess = args.guess?.trim() || "";

  try {
    // Fetch ALL custom types (there are few) and filter leniently ourselves so
    // a slightly-off toolkit slug (e.g. 'monday.com' vs 'monday') still matches.
    const [native, customAll] = await Promise.all([
      listTriggerTypes({
        toolkits: toolkit ? [toolkit] : undefined,
        keyword: query,
        limit: 12,
      }),
      listCustomTriggerTypes(),
    ]);

    const normSlug = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
    const tkNorm = toolkit ? normSlug(toolkit) : "";
    // Lenient toolkit scoping: keep types whose toolkit overlaps the guess
    // either way; if that leaves nothing, fall back to all custom types.
    let custom = customAll;
    if (tkNorm) {
      const byToolkit = customAll.filter((t) => {
        const ttk = normSlug(t.toolkit);
        return ttk.includes(tkNorm) || tkNorm.includes(ttk);
      });
      if (byToolkit.length) custom = byToolkit;
    }
    const kw = (query ?? "").toLowerCase();
    if (kw) {
      custom = custom.filter(
        (t) =>
          t.slug.toLowerCase().includes(kw) ||
          t.name.toLowerCase().includes(kw) ||
          t.description.toLowerCase().includes(kw)
      );
    }

    // Required-config lookup for whichever slug we settle on. Case-INSENSITIVE:
    // catalog slugs are UPPER_SNAKE but the model often emits lowercase — a
    // correct-but-lowercased guess must still match (and be canonicalized).
    const pick = (slug: string): ResolvedTrigger => {
      const up = slug.toUpperCase();
      const n = native.find((t) => t.slug.toUpperCase() === up);
      if (n) return { slug: n.slug, requiredConfig: requiredKeysOf(n.configSchema) };
      const c = custom.find((t) => t.slug.toUpperCase() === up);
      if (c) return { slug: c.slug, requiredConfig: c.configSchema.required ?? [] };
      return { slug, requiredConfig: [] };
    };

    // If the model's guess is a real catalog entry (any casing), trust it —
    // pick() canonicalizes to the catalog's exact slug.
    const known = new Set<string>(
      [...native.map((t) => t.slug), ...custom.map((t) => t.slug)].map((s) =>
        s.toUpperCase()
      )
    );
    if (guess && known.has(guess.toUpperCase())) return pick(guess);

    // Otherwise take the best-ranked candidate. listTriggerTypes already ranks
    // natives by the keyword; prefer a native match, then a custom (polling) one.
    if (query || !guess) {
      if (native.length) return pick(native[0].slug);
      if (custom.length) return pick(custom[0].slug);
    }
  } catch {
    // discovery unavailable (e.g. no COMPOSIO_API_KEY) — fall back to the guess
  }
  return { slug: guess, requiredConfig: [] };
}

export function narrowTrigger(o: FlatTriggerFields): CompiledTrigger {
  switch (o.triggerKind) {
    case "schedule": {
      const t: CompiledTrigger = { kind: "schedule" };
      if (o.everyMs && o.everyMs > 0) t.everyMs = o.everyMs;
      else if (o.cron) t.cron = o.cron.trim();
      else t.everyMs = 3_600_000; // safe default: hourly
      if (o.tz) t.tz = o.tz;
      return t;
    }
    case "composio": {
      const filter = parseFilter(o.composioFilter);
      // triggerType must be the resolved slug by now. An empty slug would
      // register a rule that can never fire — fail loudly instead (both the
      // /automate and /team compile paths surface this as a chat error).
      const triggerType = (o.composioTriggerType || "").trim();
      if (!triggerType) {
        throw new Error(
          "couldn't resolve an event trigger for this automation — name the app and event more specifically."
        );
      }
      return {
        kind: "composio",
        triggerType,
        ...(filter && Object.keys(filter).length ? { filter } : {}),
      };
    }
    case "webhook":
      return { kind: "webhook" };
    case "chat": {
      // Validate the regex NOW — an invalid pattern stored here is silently
      // skipped at match time (the rule looks active but never fires). Fall
      // back to a literal-escaped match of the raw text instead of failing.
      let pattern = o.chatPattern || ".*";
      let flags = o.chatFlags || "i";
      // Reject invalid OR ReDoS-shaped patterns at compile time — same bound the
      // runtime matcher enforces (safeRegex) — degrading to a literal,
      // case-insensitive match so the rule still fires predictably and safely.
      if (isDangerousRegex(pattern) || !compilesOk(pattern, flags)) {
        pattern = pattern.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
        flags = "i";
      }
      return { kind: "chat", pattern, flags };
    }
  }
}

// Narrow a compiled plan value object into a runtime PlanValue, or null when
// the discriminated field it needs is missing.
function narrowPlanValue(
  v: z.infer<typeof planValueSchema> | null | undefined
): PlanValue | null {
  if (!v) return null;
  if (v.from === "const") return { from: "const", value: v.value ?? "" };
  if (v.from === "event") return v.path ? { from: "event", path: v.path.trim() } : null;
  if (v.from === "ai") return v.prompt ? { from: "ai", prompt: v.prompt } : null;
  return null;
}

function narrowPlanSteps(o: z.infer<typeof compileSchema>): PlanStep[] {
  return (o.planSteps ?? [])
    .map((s): PlanStep | null => {
      if (s.op === "send") {
        const text = narrowPlanValue(s.text);
        return text ? { op: "send", text } : null;
      }
      if (s.op === "vfs_write") {
        const content = narrowPlanValue(s.content);
        return content && s.path ? { op: "vfs_write", path: s.path, content } : null;
      }
      if (s.op === "vfs_append") {
        const content = narrowPlanValue(s.content);
        return content && s.path ? { op: "vfs_append", path: s.path, content } : null;
      }
      if (s.op === "composio") {
        if (!s.tool) return null;
        const args: Record<string, PlanValue> = {};
        for (const pair of s.args ?? []) {
          const val = narrowPlanValue(pair.value);
          if (pair.key && val) args[pair.key] = val;
        }
        return {
          op: "composio",
          tool: s.tool.trim(),
          args,
          ...(s.connectedAccountId ? { connectedAccountId: s.connectedAccountId } : {}),
        };
      }
      return null;
    })
    .filter((s): s is PlanStep => s !== null);
}

function narrowAction(o: z.infer<typeof compileSchema>): AutomationAction {
  if (o.actionMode === "light") {
    const steps: LightStep[] = (o.lightSteps ?? [])
      .map((s): LightStep | null => {
        if (s.op === "send") return { op: "send", text: s.text ?? "" };
        if (s.op === "vfs_write")
          return { op: "vfs_write", path: s.path ?? "", content: s.content ?? "" };
        if (s.op === "vfs_append")
          return { op: "vfs_append", path: s.path ?? "", content: s.content ?? "" };
        return null;
      })
      .filter((s): s is LightStep => s !== null);
    // If the model picked light but gave no usable steps, fall back to a job.
    if (steps.length > 0) return { mode: "light", steps };
  }
  if (o.actionMode === "plan") {
    const steps = narrowPlanSteps(o);
    // Only accept the plan when EVERY emitted step narrowed cleanly. A partial
    // plan (some steps dropped for missing fields) would silently do less than
    // the user asked — fall back to a job, where the agent does the whole task.
    if (steps.length > 0 && steps.length === (o.planSteps ?? []).length) {
      return { mode: "plan", steps };
    }
  }
  return {
    mode: "job",
    instruction: o.instruction || "Handle the triggering event.",
    deep: o.deep ?? false,
    ...(o.skills && o.skills.length
      ? { skills: o.skills.filter((s) => (KNOWN_SKILLS as readonly string[]).includes(s)) }
      : {}),
  };
}

export async function compileAutomationStep(args: {
  spec: string;
  retry?: boolean;
}): Promise<CompiledAutomation> {
  "use step";

  const llm = buildLlmArgs({
    purpose: args.retry ? "meta-pro" : "meta",
    temperature: 0.2,
  });

  const result = await generateObject({
    model: (llm as any).model,
    ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
    ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
    schema: compileSchema,
    system: buildSystem(),
    prompt: `Compile this automation request:\n\n${args.spec}`,
  });

  const o = result.object;

  // Resolve the exact Composio trigger slug from the LIVE catalog (native +
  // custom polling types like monday.com) before narrowing — no hardcoded list.
  // Then reconcile the extracted subscription config against the slug's required
  // keys so a config-bearing trigger (e.g. monday needs board_id) subscribes in
  // one shot rather than erroring on a missing key.
  let triggerConfig = parseFilter(o.triggerConfig);
  if (o.triggerKind === "composio") {
    const resolved = await resolveComposioTriggerSlug({
      toolkit: o.composioToolkit,
      query: o.composioQuery,
      guess: o.composioTriggerType,
    });
    // A silently-empty slug would register a rule that can never fire; fail the
    // compile with an actionable message instead (surfaced by the /automate
    // handler, and the retry pass takes a second shot on the sharper model).
    if (!resolved.slug) {
      throw new Error(
        `couldn't find an event trigger for "${o.composioToolkit ?? "?"}" matching "${
          o.composioQuery ?? o.composioTriggerType ?? "?"
        }" — is that app connected? Try naming the app and event more specifically.`
      );
    }
    o.composioTriggerType = resolved.slug;
    if (resolved.requiredConfig.length) {
      triggerConfig = normalizeConfigKeys(triggerConfig ?? {}, resolved.requiredConfig);
    }
  }

  return {
    name: o.name?.trim() || "Untitled automation",
    summary: o.summary?.trim() || o.name?.trim() || "Automation",
    trigger: narrowTrigger(o),
    action: narrowAction(o),
    ...(triggerConfig && Object.keys(triggerConfig).length ? { triggerConfig } : {}),
  };
}
