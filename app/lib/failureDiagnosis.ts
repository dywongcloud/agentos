// app/lib/failureDiagnosis.ts
//
// Turn raw failure text (job errors, automation run errors, eval errorMessage)
// into something a non-developer can act on: a one-line cause + concrete fix
// steps. Pure string matching, no I/O — safe to import from server components
// and workflow code alike.

export type FailureDiagnosis = {
  // short, plain-English root cause
  cause: string;
  // "transient" failures usually fix themselves on retry; "config" needs a
  // human to change something; "bug" means the spec/prompt itself is off.
  kind: "transient" | "config" | "bug";
  // ordered, concrete steps the user can take
  fix: string[];
};

type Rule = {
  match: RegExp;
  diagnose: (text: string) => FailureDiagnosis;
};

const APP_RE =
  /\b(gmail|googlesheets|google_sheets|googlecalendar|google_calendar|googledrive|google_drive|notion|slack|github|linear|monday|composio_search|twitter|exa|firecrawl|perplexity)\b/i;

function appFrom(text: string): string | null {
  const m = text.match(APP_RE);
  return m ? m[1].toLowerCase().replace(/_/g, "") : null;
}

const RULES: Rule[] = [
  {
    // expired / revoked connections — by far the most common real failure
    match:
      /\bEXPIRED\b|re-?auth|reauthorize|invalid_grant|token (?:has )?expired|connection .*(?:expired|inactive|revoked)|account .*not connected|no connected account/i,
    diagnose: (text) => {
      const app = appFrom(text);
      return {
        cause: `The ${app ?? "app"} connection has expired or was revoked, so the agent couldn't act on your behalf.`,
        kind: "config",
        fix: [
          `Reconnect: message the bot "connect ${app ?? "<app>"}" (or use the integrations page) and complete the OAuth flow.`,
          "Re-run the failed item: \"/team run <id>\" for a workforce, \"/automate run <id>\" for an automation, or just ask again in chat.",
          "Note: a connection showing EXPIRED can still work — tokens refresh on use. Only reconnect if the run actually failed with an auth error.",
        ],
      };
    },
  },
  {
    match: /\b401\b|\bunauthorized\b|\bauthentication\b.*(?:fail|error|invalid)|invalid api key|api key.*(?:invalid|missing)/i,
    diagnose: (text) => {
      const app = appFrom(text);
      return {
        cause: `Authentication was rejected${app ? ` by ${app}` : ""} — the credential is wrong or missing.`,
        kind: "config",
        fix: [
          app
            ? `Reconnect ${app}: message the bot "connect ${app}" and complete the OAuth flow.`
            : "Check the API key for the failing service in Vercel → Settings → Environment Variables, then redeploy.",
          "Re-run the failed item once the credential is fixed.",
        ],
      };
    },
  },
  {
    match: /\b403\b|\bforbidden\b|insufficient.*(?:scope|permission)|permission denied/i,
    diagnose: (text) => {
      const app = appFrom(text);
      return {
        cause: `The ${app ?? "connected"} account doesn't have permission for this action (missing scope or access).`,
        kind: "config",
        fix: [
          `Reconnect ${app ?? "the app"} and approve ALL requested permissions during OAuth.`,
          "If it's a shared resource (sheet, board, repo), confirm the connected account has access to it.",
          "Re-run the failed item.",
        ],
      };
    },
  },
  {
    match: /\b429\b|rate.?limit|too many requests|quota exceeded|insufficient_quota/i,
    diagnose: () => ({
      cause: "A rate limit or quota was hit. This is usually temporary.",
      kind: "transient",
      fix: [
        "Wait a few minutes and re-run — nothing is broken.",
        "If it recurs on a schedule, space the trigger out (e.g. hourly instead of every minute).",
        "If it's an LLM quota error, check the provider's billing/usage dashboard.",
      ],
    }),
  },
  {
    match: /\b(?:529|503|502)\b|overloaded|service unavailable|model.*(?:unavailable|not available)|provider.*(?:outage|down)|temporarily unavailable/i,
    diagnose: () => ({
      cause: "The model provider or an upstream service was temporarily down or overloaded.",
      kind: "transient",
      fix: [
        "Re-run — these outages usually clear within minutes.",
        "If a specific model keeps failing, switch the affected purpose to another model in app/lib/modelRouting.ts DEFAULTS.",
      ],
    }),
  },
  {
    match: /timed? ?out|deadline|aborted|ETIMEDOUT|context.*(?:length|window).*exceed|maximum.*tokens/i,
    diagnose: () => ({
      cause: "The run took too long or produced/consumed more than fits in one turn.",
      kind: "transient",
      fix: [
        "Re-run — transient slowness often clears.",
        "If it keeps timing out, narrow the task: smaller date range, fewer items, or split one big team stage into two.",
        "For huge inputs, ask the agent to summarize in chunks instead of all at once.",
      ],
    }),
  },
  {
    match: /COMPOSIO_API_KEY|OPENAI_API_KEY|ANTHROPIC_API_KEY|TELEGRAM_BOT_TOKEN|missing (?:required )?env|environment variable.*(?:missing|not set)/i,
    diagnose: (text) => {
      const m = text.match(/\b([A-Z][A-Z0-9_]{4,})\b/);
      return {
        cause: `A required environment variable${m ? ` (${m[1]})` : ""} is missing from the deployment.`,
        kind: "config",
        fix: [
          `Add ${m ? m[1] : "the variable"} in Vercel → Project → Settings → Environment Variables.`,
          "Redeploy (vercel deploy --prod), then re-run the failed item.",
        ],
      };
    },
  },
  {
    match: /no usable stages|no agents? (?:found|resolved)|unknown agent/i,
    diagnose: () => ({
      cause: "The team spec compiled to stages, but none of the named agents could be resolved.",
      kind: "bug",
      fix: [
        "Recreate the team and name the agents explicitly in the description (e.g. \"Researcher then Writer\").",
        "Check \"/agents\" to see what exists — names are matched case-insensitively.",
      ],
    }),
  },
  {
    match: /propertyNames|z\.record|structured output.*(?:invalid|fail)|response_format.*invalid|invalid schema/i,
    diagnose: () => ({
      cause: "The model was asked for structured output with a schema the provider rejects (e.g. a map/record type).",
      kind: "bug",
      fix: [
        "This is a code-level issue: replace z.record fields with a JSON-encoded string field and parse after.",
        "Re-run after the schema fix is deployed.",
      ],
    }),
  },
  {
    match: /secret.?token|signature.*(?:mismatch|invalid)|webhook.*(?:secret|signature)/i,
    diagnose: () => ({
      cause: "A webhook arrived with a wrong or missing secret — the sender and this app are out of sync.",
      kind: "config",
      fix: [
        "For an agent Telegram bot: re-bind it (\"/agent bind <agentId> <botToken>\") to reset the webhook + secret.",
        "For Composio webhooks: confirm COMPOSIO_WEBHOOK_SECRET matches the dashboard value, then redeploy.",
      ],
    }),
  },
  {
    match: /missing required config|required (?:config|field).*missing|board_id|config.*required/i,
    diagnose: () => ({
      cause: "The trigger needs configuration values (e.g. a Monday board_id) that weren't supplied.",
      kind: "config",
      fix: [
        "Recreate the trigger and fill in every required config field in the builder.",
        "For Monday triggers, the board id is the number in the board's URL.",
      ],
    }),
  },
  {
    match: /ENOTFOUND|ECONNREFUSED|ECONNRESET|fetch failed|network error|socket hang ?up/i,
    diagnose: () => ({
      cause: "A network call to an external service failed mid-flight.",
      kind: "transient",
      fix: [
        "Re-run — these are almost always one-off network blips.",
        "If one service fails consistently, check its status page.",
      ],
    }),
  },
  {
    match: /JSON\.parse|Unexpected token|is not valid JSON|SyntaxError/i,
    diagnose: () => ({
      cause: "Something returned malformed JSON where structured data was expected.",
      kind: "transient",
      fix: [
        "Re-run — model output formatting glitches are usually one-offs.",
        "If it repeats on the same step, the prompt for that step likely needs a stricter output instruction.",
      ],
    }),
  },
];

// Diagnose a failure from its error/result text. Returns null when there is
// nothing actionable to say (no text, or no pattern matched confidently).
export function diagnoseFailure(text: string | null | undefined): FailureDiagnosis | null {
  if (!text || !text.trim()) return null;
  for (const rule of RULES) {
    if (rule.match.test(text)) return rule.diagnose(text);
  }
  return {
    cause: "The run failed with an error that doesn't match a known pattern.",
    kind: "bug",
    fix: [
      "Read the raw error above — it often names the failing app or step.",
      "Re-run once: \"/team run <id>\", \"/automate run <id>\", or re-ask in chat.",
      "If it fails the same way twice, the spec/prompt likely needs adjusting — rephrase what you asked for.",
    ],
  };
}
