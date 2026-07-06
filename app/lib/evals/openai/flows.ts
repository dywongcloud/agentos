// app/lib/evals/openai/flows.ts
//
// Registry of every AI inference flow in the system, expressed as an
// OpenAI Evals API spec. Each entry is a "faithful re-run": the flow's
// real system prompt + real structured-output schema + a small golden
// dataset, sampled fresh by the OpenAI Evals API (responses data source)
// and graded server-side by a score_model grader.
//
// Why this lives apart from app/lib/evals/{cases,recorder}.ts: those record
// graders over *live* /job traffic (Redis-backed, our own grader code).
// This module instead drives OpenAI's hosted Evals product so each flow's
// model+prompt+schema contract is exercised on demand and the results land
// back in the same dashboard (see runner.ts → putRun).
//
// Faithfulness notes / substitutions:
//   - The OpenAI Evals API can only *sample* OpenAI models. Flows whose real
//     model is Gemini (chat, reaction, browser-brain, depth head-2) or Claude
//     (code-project) are run on an OpenAI stand-in (`evalPurpose`) and tagged
//     with `realProvider` so the dashboard shows the substitution honestly.
//   - Audio flows (voice transcription, TTS) are not text-evaluable via the
//     Responses Evals API; they're marked `audio` and surfaced as info rows
//     rather than faked into a text test.

import type { Purpose } from "@/app/lib/modelRouting";

export const MODALITY_IDS = [
  "code-rust",
  "code-rust-zk",
  "code-ui-nextjs-ts",
  "code-generic",
  "latex-pdf",
  "research",
  "generic",
] as const;

export const MEMORY_KINDS = [
  "directory",
  "command",
  "preference",
  "fact",
  "code_snippet",
  "workflow",
  "person",
  "project",
  "credential_hint",
  "favorite_app",
  "other",
] as const;

type JsonSchema = Record<string, unknown>;

export type FlowEvalSpec = {
  id: string;
  name: string;
  // Short human description of what the flow does — shown in the dashboard.
  blurb: string;
  // The provider that actually runs this flow in prod. When it isn't openai,
  // the eval substitutes an OpenAI model (evalPurpose) and we say so.
  realProvider: "openai" | "google" | "anthropic";
  // Purpose used to resolve the OpenAI model the eval samples. Kept in sync
  // with the flow's real purpose where the flow is already OpenAI.
  evalPurpose: Purpose;
  // The flow's real entry point, for context in the dashboard.
  trigger: string;
  // Audio flows can't be run through the text Responses Evals API.
  audio?: boolean;
  // Whether the flow emits structured JSON (strict json_schema) or free text.
  structured: boolean;
  // Strict JSON schema for structured flows (drives sampling_params.text.format).
  outputSchema?: { name: string; schema: JsonSchema };
  // Faithful system prompt (copied/condensed from the flow's source).
  system: string;
  // Per-item input fields → JSON-schema property types. Used to build the
  // eval's data_source_config.item_schema and the run dataset rows.
  itemFields: Record<string, "string" | "number" | "boolean">;
  // The user message template; may reference {{item.<field>}}.
  userTemplate: string;
  // Golden dataset rows (each keyed by itemFields).
  golden: Array<Record<string, string | number | boolean>>;
  // Score-model grader rubric. References {{item.*}} and, for structured
  // flows, {{sample.output_json.*}}; for free-form, {{sample.output_text}}.
  graderRubric: string;
  // Optional deterministic check on the raw output text.
  stringCheck?: {
    name: string;
    input: string;
    operation: "eq" | "ne" | "like" | "ilike";
    reference: string;
  };
};

// --- small json-schema helpers (strict mode) ----------------------------
// OpenAI strict structured outputs require: additionalProperties:false on
// every object and `required` listing every property (nullable via union).

const str: JsonSchema = { type: "string" };
const num: JsonSchema = { type: "number" };
const bool: JsonSchema = { type: "boolean" };
const nullable = (s: JsonSchema): JsonSchema => ({
  ...s,
  type: Array.isArray(s.type) ? s.type : [s.type as string, "null"],
});
const arr = (items: JsonSchema): JsonSchema => ({ type: "array", items });
const enumStr = (vals: readonly string[]): JsonSchema => ({
  type: "string",
  enum: [...vals],
});
function obj(props: Record<string, JsonSchema>): JsonSchema {
  return {
    type: "object",
    additionalProperties: false,
    required: Object.keys(props),
    properties: props,
  };
}

// --- the flows ----------------------------------------------------------

export const FLOWS: FlowEvalSpec[] = [
  // 1. main conversational agent (Gemini in prod) -----------------------
  {
    id: "chat-reply",
    name: "Chat reply (main agent)",
    blurb: "Plain Telegram conversational turn — the default non-/job agent reply.",
    realProvider: "google",
    evalPurpose: "smart",
    trigger: "Telegram message (non-command)",
    structured: false,
    system: [
      "You are a warm, sharp personal assistant texting with the user on",
      "Telegram. Reply like a knowledgeable friend: concise, direct, lowercase",
      "is fine, contractions encouraged, no corporate filler. Answer the actual",
      "question first, then any needed detail. Don't pad, don't over-hedge, and",
      "don't announce what you're about to do — just do it.",
    ].join("\n"),
    itemFields: { message: "string" },
    userTemplate: "{{item.message}}",
    golden: [
      { message: "what's the difference between a process and a thread, quick version" },
      { message: "i'm feeling kinda burned out on this project ngl" },
    ],
    graderRubric: [
      "You are grading a personal-assistant chat reply.",
      "User message: {{item.message}}",
      "Assistant reply: {{sample.output_text}}",
      "Score 1-7. A great reply directly addresses the message, is concise and",
      "natural (texting a friend, not a press release), and is correct where",
      "factual. Penalize corporate filler, hedging, refusing without reason, or",
      "missing the point. 1=useless/off, 4=acceptable, 7=excellent.",
    ].join("\n"),
  },

  // 2. clarifier --------------------------------------------------------
  {
    id: "clarify-prompt",
    name: "Clarifier",
    blurb: "Infers intent and surfaces assumptions for a /job — never asks the user back.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "/job and /deep dispatch (first phase)",
    structured: true,
    outputSchema: {
      name: "clarify",
      schema: obj({
        assumptions: arr(str),
        clarity: enumStr(["clear", "mostly_clear", "ambiguous"]),
      }),
    },
    system: [
      "You are the clarifier for an autonomous agent. The agent will execute the",
      "user's request without asking back-and-forth questions. Your job is to",
      "infer the user's most likely intent and surface the assumptions you are",
      "making so they are visible in the agent's reasoning log.",
      "",
      "Rules:",
      "1. NEVER request more information from the user. Always assume.",
      "2. When the prompt is vague, pick the most-likely interpretation given",
      "   typical user intent and list every non-obvious choice as an assumption.",
      "3. When the prompt is detailed, your assumption list may be short or empty.",
      "4. Keep each assumption to one short sentence.",
      "",
      "Output JSON: assumptions (array of strings, may be empty), clarity",
      "('clear' | 'mostly_clear' | 'ambiguous' — diagnostic only).",
    ].join("\n"),
    itemFields: { prompt: "string" },
    userTemplate: "User request:\n{{item.prompt}}",
    golden: [
      { prompt: "make me a landing page" },
      { prompt: "Using my gmails, summarize everything this week then save the summary to my vfs as a markdown file" },
    ],
    graderRubric: [
      "Grade the clarifier output for an autonomous agent.",
      "User request: {{item.prompt}}",
      "Output assumptions: {{sample.output_json.assumptions}}",
      "Output clarity: {{sample.output_json.clarity}}",
      "Score 1-7. Good output: NEVER asks the user a question; lists concrete,",
      "non-obvious assumptions for vague prompts and few/none for detailed ones;",
      "clarity label matches the prompt's actual ambiguity. Penalize asking for",
      "more info, generic filler assumptions, or a clarity label that's clearly",
      "wrong. 4=acceptable, 7=excellent.",
    ].join("\n"),
  },

  // 3. planner ----------------------------------------------------------
  {
    id: "plan-agent",
    name: "Planner",
    blurb: "Classifies modality and emits a concrete ordered plan for a /job.",
    realProvider: "openai",
    evalPurpose: "reasoning",
    trigger: "/job workflow (after clarify)",
    structured: true,
    outputSchema: {
      name: "plan",
      schema: obj({
        modality: enumStr(MODALITY_IDS),
        plan: arr(
          obj({
            id: str,
            kind: enumStr(["research", "code", "write", "tool", "verify"]),
            description: str,
          })
        ),
        rationale: str,
      }),
    },
    system: [
      "You are a senior planner for an autonomous agent that must produce",
      "high-fidelity, fully functional output — never skeletons, stubs, or",
      "placeholders.",
      "",
      "Output a structured plan:",
      "  modality — what kind of output is being produced. Choose from:",
      `             ${MODALITY_IDS.join(", ")}.`,
      "  plan     — ordered concrete steps; each step's 'description' should be",
      "             specific enough that an executor knows what to do.",
      "  rationale — one paragraph on why this plan, in your own voice.",
    ].join("\n"),
    itemFields: { prompt: "string" },
    userTemplate: "User request:\n{{item.prompt}}",
    golden: [
      { prompt: "Build a Rust CLI that parses a CSV of transactions and prints a monthly summary table." },
      { prompt: "Research the current state of zero-knowledge rollups and write a detailed comparison of the top 3." },
    ],
    graderRubric: [
      "Grade an autonomous agent's plan.",
      "User request: {{item.prompt}}",
      "Chosen modality: {{sample.output_json.modality}}",
      "Plan steps: {{sample.output_json.plan}}",
      "Rationale: {{sample.output_json.rationale}}",
      "Score 1-7. Good: modality matches the request (code-* for code, research",
      "for research, etc.); steps are concrete and ordered with no stubs; rationale",
      "is sound. Penalize a wrong modality, vague/placeholder steps, or a plan that",
      "wouldn't actually produce the asked-for output. 4=acceptable, 7=excellent.",
    ].join("\n"),
  },

  // 4. deep orchestrator ------------------------------------------------
  {
    id: "orchestrator-deep",
    name: "Deep orchestrator",
    blurb: "Per-iteration decision loop for /deep: picks the next action toward the goal.",
    realProvider: "openai",
    evalPurpose: "meta",
    trigger: "/deep and /extended orchestration loop",
    structured: true,
    outputSchema: {
      name: "orchestrate",
      schema: obj({
        action: enumStr(["research", "execute", "synthesize", "compute", "done"]),
        reasoning: str,
        goal: str,
        instructions: str,
        query: nullable(str),
        finalSynthesis: nullable(str),
        modality: nullable(enumStr(MODALITY_IDS)),
      }),
    },
    system: [
      "You are the orchestrator for an autonomous agent running in pro-extended",
      "deep mode. Each turn you read the goal, the user's assumptions, and the",
      "outputs of completed subtasks, then decide what to do NEXT. Choose ONE",
      "action:",
      "  research   — run a web search subagent for a specific query.",
      "  execute    — run the executor agent with a concrete instruction.",
      "  synthesize — combine prior subtask outputs into a longer writeup.",
      "  compute    — hand a deterministic numeric/parsing task to a Python",
      "               code interpreter.",
      "  done       — you are satisfied; put the COMPLETE final answer in",
      "               finalSynthesis and set modality.",
      "Don't pick 'done' until the answer is truly complete. Don't over-research.",
      "If a verifier note is present, your next action must address it first.",
    ].join("\n"),
    itemFields: { goal: "string", subtasks: "string" },
    userTemplate:
      "User goal:\n{{item.goal}}\n\nCompleted subtasks so far:\n{{item.subtasks}}",
    golden: [
      {
        goal: "Estimate the total addressable market for AI coding assistants in 2027 with a defensible bottom-up model.",
        subtasks: "No subtasks completed yet.",
      },
      {
        goal: "Compute the eigenvalues of a 3x3 matrix the user provided and explain their meaning.",
        subtasks: "[1] (research) background on eigenvalues: gathered definitions and use cases.",
      },
    ],
    graderRubric: [
      "Grade the deep orchestrator's next-action decision.",
      "Goal: {{item.goal}}",
      "Subtasks so far: {{item.subtasks}}",
      "Decision action: {{sample.output_json.action}}",
      "Reasoning: {{sample.output_json.reasoning}}",
      "Instructions: {{sample.output_json.instructions}}",
      "Score 1-7. Good: the action is the sensible next step given the state",
      "(e.g. 'compute' for a deterministic numeric task, 'research' when facts are",
      "missing, not 'done' prematurely); instructions are concrete. Penalize a",
      "poorly-justified or clearly-wrong action choice. 4=acceptable, 7=excellent.",
    ].join("\n"),
  },

  // 5. web search subagent ---------------------------------------------
  {
    id: "web-search-research",
    name: "Web-search subagent",
    blurb: "Answers a research query with findings + citations (web tool not invoked in eval).",
    realProvider: "openai",
    evalPurpose: "search",
    trigger: "Orchestrator action=research",
    structured: false,
    system: [
      "You are a research subagent. Given a query, produce a tight, factual",
      "answer: lead with the finding, support it with specific facts/numbers, and",
      "cite sources as bare URLs inline. Be precise; flag uncertainty honestly.",
      "(In this evaluation the live web-search tool is not invoked — answer from",
      "your own knowledge and cite where the facts would normally come from.)",
    ].join("\n"),
    itemFields: { query: "string" },
    userTemplate: "Research query:\n{{item.query}}",
    golden: [
      { query: "What is the current Ethereum block gas limit and how has it changed since 2021?" },
      { query: "Who are the leading providers of hosted vector databases as of 2025?" },
    ],
    graderRubric: [
      "Grade a research subagent's answer.",
      "Query: {{item.query}}",
      "Answer: {{sample.output_text}}",
      "Score 1-7. Good: directly answers with specific facts, names, or numbers;",
      "structured and readable; cites/attributes sources; flags uncertainty rather",
      "than bluffing. Penalize vagueness, hand-waving, or evasion. 4=acceptable.",
    ].join("\n"),
  },

  // 6. code interpreter -------------------------------------------------
  {
    id: "code-interpreter",
    name: "Code-interpreter compute",
    blurb: "Solves a deterministic numeric/parsing task (sandbox not invoked in eval).",
    realProvider: "openai",
    evalPurpose: "search",
    trigger: "Orchestrator action=compute",
    structured: false,
    system: [
      "You handle deterministic compute tasks: numeric math, parsing, statistics,",
      "simulation. Show the result clearly and state the method. (In this",
      "evaluation the live Python sandbox is not invoked — compute the answer",
      "directly and show your working.)",
    ].join("\n"),
    itemFields: { task: "string" },
    userTemplate: "Compute task:\n{{item.task}}",
    golden: [
      { task: "What is the standard deviation of [4, 8, 15, 16, 23, 42]? Show the steps." },
      { task: "How many trailing zeros are in 100! (100 factorial)? Explain why." },
    ],
    graderRubric: [
      "Grade a compute answer for correctness.",
      "Task: {{item.task}}",
      "Answer: {{sample.output_text}}",
      "Score 1-7 PRIMARILY on numeric correctness of the final result, then on",
      "whether the method shown is valid. A wrong final number caps the score at",
      "3 regardless of presentation. 4=correct with rough working, 7=correct and",
      "clearly explained.",
    ].join("\n"),
  },

  // 7. depth reviewer ---------------------------------------------------
  {
    id: "review-depth",
    name: "Depth reviewer",
    blurb: "Scores a deep-job draft on insight/data-density/coverage/rigor and lists gaps.",
    realProvider: "openai",
    evalPurpose: "reasoning",
    trigger: "Deep job after synthesis",
    structured: true,
    outputSchema: {
      name: "review_depth",
      schema: obj({
        insight: num,
        data_density: num,
        coverage: num,
        rigor: num,
        gaps: arr(str),
        verdict_reason: str,
      }),
    },
    system: [
      "You are a demanding research editor reviewing a draft answer to a",
      "high-stakes request. The draft already passed a correctness check — judge",
      "DEPTH, not correctness. Score 1-10 on four axes:",
      "  insight      — non-obvious connections, second-order effects, tradeoffs.",
      "  data_density — concrete numbers, dates, named examples, citations.",
      "  coverage     — are the important angles/sub-questions addressed?",
      "  rigor        — sound reasoning, sourced claims, no hand-waving.",
      "Be a tough grader. 8+ means genuinely excellent. List SPECIFIC, actionable",
      "gaps (name the exact data point/angle to add); empty if genuinely excellent.",
    ].join("\n"),
    itemFields: { request: "string", draft: "string" },
    userTemplate: "Original request:\n{{item.request}}\n\nDraft answer:\n{{item.draft}}",
    golden: [
      {
        request: "Compare Postgres and MySQL for a high-write analytics workload.",
        draft: "Both are popular databases. Postgres is good and has many features. MySQL is fast and widely used. You should pick based on your needs and team experience.",
      },
      {
        request: "Compare Postgres and MySQL for a high-write analytics workload.",
        draft: "For high-write analytics, Postgres' MVCC and table partitioning (declarative, since v10) plus BRIN indexes suit append-heavy fact tables; its parallel query and columnar extensions (cstore_fdw, Citus) help scans. MySQL/InnoDB's clustered PK gives fast point writes but secondary-index write amplification hurts wide analytic tables; its query planner is weaker for multi-join star schemas. Benchmark: on a 500M-row TPC-H-like load, Postgres+Citus showed ~2.3x scan throughput vs MySQL 8 in [cite]. Recommend Postgres unless the team's ops expertise is MySQL-only.",
      },
    ],
    graderRubric: [
      "You are grading whether the depth-reviewer scored a draft sensibly.",
      "Request: {{item.request}}",
      "Draft under review: {{item.draft}}",
      "Reviewer scores — insight {{sample.output_json.insight}}, data_density",
      "{{sample.output_json.data_density}}, coverage {{sample.output_json.coverage}},",
      "rigor {{sample.output_json.rigor}}. Gaps: {{sample.output_json.gaps}}.",
      "Score 1-7. Good: the scores track the draft's actual quality (a vague draft",
      "gets low scores, a dense well-sourced draft gets high ones) and the gaps are",
      "specific and actionable. Penalize miscalibrated scores or generic gaps.",
    ].join("\n"),
  },

  // 8. subtask compaction ----------------------------------------------
  {
    id: "subtask-compaction",
    name: "Subtask compaction",
    blurb: "Compresses older subtask results into one dense paragraph for deep mode.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "Deep mode when subtask history grows",
    structured: false,
    system: [
      "You compress prior subtask outputs into ONE dense paragraph that preserves",
      "every load-bearing fact, number, and decision while dropping narration and",
      "redundancy. No headers, no preamble — just the compressed paragraph.",
    ].join("\n"),
    itemFields: { subtasks: "string" },
    userTemplate: "Subtask outputs to compress:\n{{item.subtasks}}",
    golden: [
      {
        subtasks:
          "[1] Found that the API rate limit is 100 req/min on the free tier and 1000 on pro. [2] The pro tier costs $49/mo. [3] Discovered the webhook signature uses HMAC-SHA256 with the account secret. [4] Latency averaged 120ms in us-east, 340ms in eu-west.",
      },
      {
        subtasks:
          "[1] Population of the city is 2.1M (2023). [2] Median home price is $640k, up 8% YoY. [3] Two new transit lines open in 2026. [4] Unemployment is 3.4%.",
      },
    ],
    graderRubric: [
      "Grade a compaction.",
      "Original subtask outputs: {{item.subtasks}}",
      "Compressed paragraph: {{sample.output_text}}",
      "Score 1-7. Good: ONE paragraph, preserves every key number/fact/decision,",
      "removes filler, materially shorter than the input. Penalize dropping a",
      "load-bearing fact, inventing facts, or barely compressing. 4=acceptable.",
    ].join("\n"),
  },

  // 9. summarize chat ---------------------------------------------------
  {
    id: "summarize-chat",
    name: "Chat summarizer",
    blurb: "Distills a conversation into a titled summary + atomic memory facts.",
    realProvider: "openai",
    evalPurpose: "smart",
    trigger: "summarize_chat_and_remember tool",
    structured: true,
    outputSchema: {
      name: "chat_summary",
      schema: obj({
        title: str,
        summary: str,
        labels: arr(str),
        atomic_facts: arr(
          obj({
            text: str,
            kind_hint: nullable(enumStr(MEMORY_KINDS)),
          })
        ),
      }),
    },
    system: [
      "Distill a conversation into long-term memory. Produce: a short title; one",
      "paragraph summary focused on decisions, conclusions, and concrete details",
      "(not narration of the back-and-forth); up to 8 labels; and up to 8 atomic",
      "facts (each a single reusable item — a preference, command, directory,",
      "project, person, etc.) with a kind_hint.",
    ].join("\n"),
    itemFields: { transcript: "string" },
    userTemplate: "Conversation:\n{{item.transcript}}",
    golden: [
      {
        transcript:
          "1. [USER] my main repo is at ~/dev/agentos, always run tests with `pnpm test:ci`. 2. [ASSISTANT] got it. 3. [USER] also i prefer dark mode everywhere and i hate tabs, spaces only. 4. [ASSISTANT] noted. 5. [USER] we decided to ship the billing rewrite next sprint.",
      },
      {
        transcript:
          "1. [USER] can you explain how our deploy works? 2. [ASSISTANT] you push to main, CI runs, then vercel --prod aliases it. 3. [USER] right, and the staging URL is staging.acme.dev. 4. [USER] remember sarah owns the infra side.",
      },
    ],
    graderRubric: [
      "Grade a chat-summary memory extraction.",
      "Transcript: {{item.transcript}}",
      "Title: {{sample.output_json.title}}",
      "Summary: {{sample.output_json.summary}}",
      "Atomic facts: {{sample.output_json.atomic_facts}}",
      "Score 1-7. Good: faithful summary of the actual decisions/details; atomic",
      "facts capture the genuinely reusable items (directories, commands,",
      "preferences, people) with sensible kind_hints; nothing invented. Penalize",
      "missed key facts, hallucinations, or narration-style summary. 4=acceptable.",
    ].join("\n"),
  },

  // 10. memory enrichment ----------------------------------------------
  {
    id: "memory-enrichment",
    name: "Memory enrichment",
    blurb: "Structures a raw remembered note into kind/title/summary/importance/fields.",
    realProvider: "openai",
    evalPurpose: "smart",
    trigger: "remember tool + atomic extraction",
    structured: true,
    outputSchema: {
      name: "memory_enrichment",
      schema: obj({
        kind: enumStr(MEMORY_KINDS),
        title: str,
        summary: str,
        labels: arr(str),
        importance: num,
        fields: obj({
          path: nullable(str),
          command: nullable(str),
          args: nullable(arr(str)),
          when_to_use: nullable(str),
          related: nullable(arr(str)),
        }),
      }),
    },
    system: [
      "Structure a raw remembered note into a memory entry. Pick the best `kind`",
      "from the enum: directory (a filesystem path the user works in), command (a",
      "shell command/recipe), preference, fact, code_snippet, workflow, person,",
      "project, credential_hint (describe only WHERE a secret lives, never the",
      "secret), favorite_app, other. Fill title, a one-line summary, labels, and",
      "importance (0..1 = how reusable across future conversations). For",
      "directory/command kinds populate fields.path / fields.command / fields.args.",
    ].join("\n"),
    itemFields: { text: "string" },
    userTemplate: "Remember this:\n{{item.text}}",
    golden: [
      { text: "my notes live in /Users/me/Documents/notes and i open them daily" },
      { text: "to deploy run `vercel --prod --yes` from the repo root" },
    ],
    graderRubric: [
      "Grade a memory-enrichment output.",
      "Raw note: {{item.text}}",
      "kind: {{sample.output_json.kind}}, title: {{sample.output_json.title}},",
      "importance: {{sample.output_json.importance}}, fields: {{sample.output_json.fields}}",
      "Score 1-7. Good: kind matches the note (directory for a path, command for a",
      "shell recipe); for directory/command the matching field (path/command) is",
      "populated correctly; importance is reasonable. Penalize a wrong kind or an",
      "empty path/command when one is clearly present. 4=acceptable, 7=excellent.",
    ].join("\n"),
  },

  // 11. ask gpt-5 -------------------------------------------------------
  {
    id: "ask-gpt5",
    name: "Ask-GPT5 escalation",
    blurb: "Manual escalation tool for a hard standalone question.",
    realProvider: "openai",
    evalPurpose: "smart",
    trigger: "ask_gpt5 tool",
    structured: false,
    system: [
      "You are a high-effort expert answering a single hard question the calling",
      "agent escalated to you. Give a correct, complete, well-reasoned answer.",
      "Show the key reasoning succinctly; don't pad.",
    ].join("\n"),
    itemFields: { question: "string" },
    userTemplate: "{{item.question}}",
    golden: [
      { question: "Why does TCP use a three-way handshake instead of a two-way one? What failure does the third message prevent?" },
      { question: "In Rust, why can't you hold a mutable reference and an immutable reference to the same data at once? What class of bug does this rule prevent?" },
    ],
    graderRubric: [
      "Grade an expert answer to a hard question.",
      "Question: {{item.question}}",
      "Answer: {{sample.output_text}}",
      "Score 1-7 on correctness and completeness of the reasoning. A technically",
      "wrong core claim caps at 3. 4=correct but thin, 7=correct, complete, and",
      "clearly reasoned.",
    ].join("\n"),
  },

  // 12. message reaction (Gemini in prod) ------------------------------
  {
    id: "message-reaction",
    name: "Emoji reaction picker",
    blurb: "Decides whether to react to a message and with which emoji.",
    realProvider: "google",
    evalPurpose: "fast-meta",
    trigger: "After every user message (non-blocking)",
    structured: true,
    outputSchema: {
      name: "reaction",
      schema: obj({ react: bool, emoji: nullable(str) }),
    },
    system: [
      "You add emoji reactions to a friend's text messages, like on iMessage or",
      "Telegram. React SELECTIVELY — when the message is funny, exciting, sweet,",
      "impressive, sad, grateful, or surprising. Skip dry logistics, neutral",
      "questions, and plain task requests (those get a real reply, not a reaction).",
      "When unsure, don't react. Aim to react to maybe a third of messages. One",
      "emoji. When react=false, emoji is null.",
    ].join("\n"),
    itemFields: { message: "string" },
    userTemplate: 'Message from the user:\n"""{{item.message}}"""',
    golden: [
      { message: "OMG we just closed the Series A!! 🎉" },
      { message: "can you send me the q3 numbers when you get a sec" },
    ],
    graderRubric: [
      "Grade an emoji-reaction decision.",
      "Message: {{item.message}}",
      "react: {{sample.output_json.react}}, emoji: {{sample.output_json.emoji}}",
      "Score 1-7. Good: reacts to emotionally-charged messages with a fitting",
      "emoji, and does NOT react to dry logistics/task requests (returns",
      "react=false, emoji=null). Penalize reacting to neutral logistics or",
      "ignoring obvious excitement/sadness. 4=acceptable, 7=ideal selectivity.",
    ].join("\n"),
  },

  // 13. autopilot heartbeat --------------------------------------------
  {
    id: "autopilot-heartbeat",
    name: "Autopilot heartbeat",
    blurb: "Decides whether the agent should proactively message the user right now.",
    realProvider: "openai",
    evalPurpose: "meta",
    trigger: "Per-tenant cron (every minute)",
    structured: true,
    outputSchema: {
      name: "heartbeat",
      schema: obj({
        should_message: bool,
        message: nullable(str),
        reason: str,
      }),
    },
    system: [
      "You're the autopilot — a friend texting from inside an agent. Once a minute",
      "you peek at what's happened (jobs, triggers, memories, recent chat) and",
      "decide: is there something worth saying right now? Lean slightly toward",
      "reaching out when there's a REAL hook (a job finished/errored, a project is",
      "awaiting input, a relevant event came in, a previously-promised follow-up is",
      "due) but stay relaxed about silence. NEVER message for 'just checking in',",
      "weather, recaps of things they already saw, or the same reason as last time.",
      "Voice: casual texting, ≤320 chars, share the actual takeaway. When",
      "should_message=false, message is null. `reason` is an internal log line.",
    ].join("\n"),
    itemFields: { snapshot: "string" },
    userTemplate: "Snapshot:\n{{item.snapshot}}",
    golden: [
      {
        snapshot:
          "Active jobs: none. Recent: job j_ab12 (deep research on TAM) COMPLETED 2 min ago, answer ready. Last proactive: (none). Last chat: 3h ago.",
      },
      {
        snapshot:
          "Active jobs: none. Recent: nothing new. Memories: none due. Last proactive: 5 min ago ('your scrape job finished'). Last chat: 10 min ago.",
      },
    ],
    graderRubric: [
      "Grade an autopilot proactive-messaging decision.",
      "Snapshot: {{item.snapshot}}",
      "should_message: {{sample.output_json.should_message}}, message:",
      "{{sample.output_json.message}}, reason: {{sample.output_json.reason}}",
      "Score 1-7. Good: messages when there's a genuine fresh hook (e.g. a job",
      "just completed and the user hasn't seen it) and stays silent when there's",
      "nothing new or it would repeat the last proactive message. Penalize filler",
      "outreach or staying silent on an obvious real hook. 4=acceptable.",
    ].join("\n"),
  },

  // 14. ask job ---------------------------------------------------------
  {
    id: "ask-job",
    name: "Ask-job side channel",
    blurb: "Answers a question about a running job from its thought log, without touching state.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "/ask <jobId> <question>",
    structured: false,
    system: [
      "You answer a user's question about one of their running/finished agent",
      "jobs, using ONLY the provided thought-log excerpt. Be concrete and honest:",
      "if the log doesn't say, say so. Don't invent progress that isn't in the log.",
    ].join("\n"),
    itemFields: { question: "string", log: "string" },
    userTemplate: "Job thought log:\n{{item.log}}\n\nQuestion: {{item.question}}",
    golden: [
      {
        question: "is it almost done?",
        log: "clarify: 2 assumptions. plan: 5 steps, modality=research. orchestrator iter 4/28: action=synthesize. review-depth: avg 7.2, verdict=more_passes.",
      },
      {
        question: "did it actually call gmail?",
        log: "executor: composio_execute_tool(GMAIL_FETCH_EMAILS) ok, 42 messages. wrote /workspace/summary.md.",
      },
    ],
    graderRubric: [
      "Grade an answer about a job, grounded in its log.",
      "Question: {{item.question}}",
      "Log: {{item.log}}",
      "Answer: {{sample.output_text}}",
      "Score 1-7. Good: answer is supported by the log, concrete, and admits when",
      "the log doesn't say. Penalize claims not in the log or vague non-answers.",
    ].join("\n"),
  },

  // 15. depth classifier ------------------------------------------------
  {
    id: "depth-classifier",
    name: "Depth classifier",
    blurb: "Decides whether a request needs deep mode vs a normal /job.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "/job dispatch (when heuristics are uncertain)",
    structured: true,
    outputSchema: {
      name: "depth_decision",
      schema: obj({ deep: bool, reason: str }),
    },
    system: [
      "Decide whether a user request warrants DEEP mode (long, multi-step research",
      "or build, 30-60 min of compute) or a normal single-pass job. Mark deep=true",
      "for: open-ended research, complete systems/frameworks, highly technical",
      "multi-part work. Mark deep=false for: quick lookups, simple edits, short",
      "factual questions, single small tasks. Give a one-sentence reason.",
    ].join("\n"),
    itemFields: { prompt: "string" },
    userTemplate: "User request:\n{{item.prompt}}",
    golden: [
      { prompt: "what's the capital of australia" },
      { prompt: "design and implement a complete distributed rate-limiter with sliding windows, redis backing, and a full test suite" },
    ],
    graderRubric: [
      "Grade a deep-vs-normal classification.",
      "Request: {{item.prompt}}",
      "deep: {{sample.output_json.deep}}, reason: {{sample.output_json.reason}}",
      "Score 1-7. Good: deep=false for trivial lookups/edits, deep=true for large",
      "open-ended research or complete-system builds, with a sensible reason.",
      "Penalize an obviously-wrong call. 4=acceptable, 7=clearly correct.",
    ].join("\n"),
  },

  // 16. browser brain (Gemini in prod) ---------------------------------
  {
    id: "browser-goal-enrichment",
    name: "Browser brain",
    blurb: "Turns a browse goal into a concrete step-by-step navigation plan.",
    realProvider: "google",
    evalPurpose: "smart",
    trigger: "Before browser tool execution",
    structured: true,
    outputSchema: {
      name: "browse_plan",
      schema: obj({
        restated_goal: str,
        suggested_start_url: nullable(str),
        steps: arr(str),
        watch_out_for: arr(str),
        extract: str,
      }),
    },
    system: [
      "You are the planning brain for a web-browsing agent. A separate, simpler",
      "model will actually drive the browser. Think hard about the user's goal and",
      "produce a tight plan the driver can follow step by step. Be concrete and",
      "web-aware: name the page to look for, the search terms to type, which result",
      "to click, how to handle pagination, cookie walls, and logins. Assume the",
      "driver is literal — it does exactly what each step says and nothing more.",
    ].join("\n"),
    itemFields: { goal: "string" },
    userTemplate: "Goal: {{item.goal}}\n\nProduce the plan.",
    golden: [
      { goal: "Find the cheapest direct flight from SFO to Tokyo in March and note the airline and price." },
      { goal: "Get the current number of open issues on the facebook/react GitHub repo." },
    ],
    graderRubric: [
      "Grade a browser navigation plan.",
      "Goal: {{item.goal}}",
      "restated_goal: {{sample.output_json.restated_goal}}",
      "start_url: {{sample.output_json.suggested_start_url}}",
      "steps: {{sample.output_json.steps}}",
      "extract: {{sample.output_json.extract}}",
      "Score 1-7. Good: a sensible start URL, concrete ordered steps a literal",
      "driver could follow, realistic obstacles called out, and a clear extract",
      "target. Penalize vague steps or a wrong/missing start point. 4=acceptable.",
    ].join("\n"),
  },

  // 17. webhook event summarizer ---------------------------------------
  {
    id: "webhook-event-summarizer",
    name: "Webhook event formatter",
    blurb: "Turns a raw Composio webhook payload into a friendly Telegram notification.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "Incoming Composio webhook",
    structured: false,
    system: [
      "Turn a raw integration webhook event into a short, friendly Telegram",
      "notification. Lead with what happened in plain language, include the one or",
      "two details that matter, and keep it to a sentence or two. No JSON, no",
      "field dumps, no preamble.",
    ].join("\n"),
    itemFields: { trigger: "string", payload: "string" },
    userTemplate: "Trigger: {{item.trigger}}\nPayload:\n{{item.payload}}",
    golden: [
      {
        trigger: "GMAIL_NEW_MESSAGE",
        payload: '{"from":"jane@acme.com","subject":"Contract signed — ready to start","snippet":"Hi, we signed the contract..."}',
      },
      {
        trigger: "GITHUB_PULL_REQUEST",
        payload: '{"action":"opened","number":482,"title":"Fix race in webhook dispatch","user":{"login":"devon"}}',
      },
    ],
    graderRubric: [
      "Grade a webhook-to-notification rendering.",
      "Trigger: {{item.trigger}}",
      "Payload: {{item.payload}}",
      "Notification: {{sample.output_text}}",
      "Score 1-7. Good: plain-language, leads with what happened, includes the key",
      "detail(s) (sender/subject, PR title/number, etc.), short, no raw JSON.",
      "Penalize field dumps, verbosity, or burying the event. 4=acceptable.",
    ].join("\n"),
  },

  // 18. eval grader (LLM judge) ----------------------------------------
  {
    id: "eval-grader-llm",
    name: "Eval LLM judge",
    blurb: "The system's own LLM grader — judges a job output against a rubric.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "Eval suite grading",
    structured: true,
    outputSchema: {
      name: "grade",
      schema: obj({ pass: bool, score: num, notes: str }),
    },
    system: [
      "You are an evaluation grader. Given a rubric, the user's goal, and the",
      "agent's actual output, decide pass/fail, a 0..1 score, and a one-line note.",
      "Be strict but fair: judge against the rubric only. pass=true requires the",
      "output to genuinely satisfy the rubric.",
    ].join("\n"),
    itemFields: { rubric: "string", goal: "string", output: "string" },
    userTemplate:
      "Rubric: {{item.rubric}}\n\nGoal: {{item.goal}}\n\nActual output:\n{{item.output}}",
    golden: [
      {
        rubric: "The output must include a working code example, not just prose.",
        goal: "Show me how to debounce a function in JavaScript.",
        output: "Debouncing delays a call until activity stops. function debounce(fn, ms){let t;return(...a)=>{clearTimeout(t);t=setTimeout(()=>fn(...a),ms);};}",
      },
      {
        rubric: "The output must include a working code example, not just prose.",
        goal: "Show me how to debounce a function in JavaScript.",
        output: "Debouncing is a technique where you wait until the user stops doing something before running your function. It's very useful for search boxes and resize handlers.",
      },
    ],
    graderRubric: [
      "You are grading the GRADER. It judged an output against a rubric.",
      "Rubric it used: {{item.rubric}}",
      "Output it judged: {{item.output}}",
      "Its verdict — pass: {{sample.output_json.pass}}, score:",
      "{{sample.output_json.score}}, notes: {{sample.output_json.notes}}",
      "Score 1-7 on whether the grader's verdict is CORRECT for this output",
      "against that rubric (the first example has code → should pass; the second",
      "is prose-only → should fail). Penalize a wrong verdict. 4=acceptable.",
    ].join("\n"),
  },

  // 19. code project (Claude in prod) ----------------------------------
  {
    id: "code-project-claude-code",
    name: "Code project turn",
    blurb: "A coding turn in the /code sandbox (Claude in prod; codex stand-in for eval).",
    realProvider: "anthropic",
    evalPurpose: "coding",
    trigger: "/code command + ask_claude_code tool",
    structured: false,
    system: [
      "You are a senior engineer making a focused code change. Produce complete,",
      "correct, runnable code — no stubs, no TODOs, no placeholders. Explain only",
      "what's necessary. When asked for a function, return the full function.",
    ].join("\n"),
    itemFields: { task: "string" },
    userTemplate: "{{item.task}}",
    golden: [
      { task: "Write a TypeScript function `chunk<T>(arr: T[], size: number): T[][]` that splits an array into chunks of the given size. Handle size<=0 by returning []." },
      { task: "Write a Python function `is_balanced(s: str) -> bool` that returns whether the brackets (), [], {} in s are balanced and correctly nested." },
    ],
    graderRubric: [
      "Grade a code change for correctness.",
      "Task: {{item.task}}",
      "Code: {{sample.output_text}}",
      "Score 1-7 PRIMARILY on whether the code is correct and complete for the",
      "task (handles the stated edge cases, no stubs/TODOs). A bug that fails the",
      "stated edge case caps at 3. 4=works, 7=correct, complete, and clean.",
    ].join("\n"),
  },

  // 20. automation compiler --------------------------------------------
  {
    id: "automation-compiler",
    name: "Automation compiler",
    blurb:
      "Compiles a /automate natural-language request into a strict {trigger, action} rule.",
    realProvider: "openai",
    evalPurpose: "meta",
    trigger: "/automate <description>",
    structured: true,
    outputSchema: {
      name: "automation",
      schema: obj({
        name: str,
        summary: str,
        triggerKind: enumStr(["schedule", "composio", "webhook", "chat"]),
        cron: nullable(str),
        everyMs: nullable(num),
        tz: nullable(str),
        composioTriggerType: nullable(str),
        // free-form filter map can't be strict; the model emits a JSON string.
        composioFilter: nullable(str),
        chatPattern: nullable(str),
        chatFlags: nullable(str),
        actionMode: enumStr(["job", "light"]),
        instruction: nullable(str),
        deep: nullable(bool),
        skills: nullable(arr(str)),
        lightSteps: nullable(
          arr(
            obj({
              op: enumStr(["send", "vfs_write", "vfs_append"]),
              text: nullable(str),
              path: nullable(str),
              content: nullable(str),
            })
          )
        ),
      }),
    },
    system: [
      "You compile a user's natural-language automation request into a strict",
      "structured rule of the form { trigger, action }. The system will run the",
      "action as a durable, fault-tolerant workflow whenever the trigger fires.",
      "",
      "Pick exactly ONE triggerKind:",
      "  schedule  — time-based. Set `cron` (standard 5-field) for calendar",
      "              cadences ('every weekday 9am' → '0 9 * * 1-5') OR `everyMs`",
      "              for fixed intervals ('every 10 min' → 600000). Set `tz` to an",
      "              IANA zone when local time is implied; never set both cron and",
      "              everyMs.",
      "  composio  — an external app event. Set `composioTriggerType` to one of:",
      "              GMAIL_NEW_GMAIL_MESSAGE, GITHUB_PULL_REQUEST_EVENT,",
      "              SLACK_NEW_MESSAGE. Put narrowing constraints in",
      "              `composioFilter` (a JSON object string of substrings that",
      "              must appear in the payload, e.g. {\"from\":\"alice@acme.com\"}).",
      "  webhook   — fires when an external system POSTs to a minted URL.",
      "  chat      — fires when an inbound chat message matches a regex. Set",
      "              `chatPattern` (a JS regex source) and optional `chatFlags`.",
      "",
      "Then pick the action. actionMode 'light' is ONLY for trivially simple pure",
      "send / virtual-file ops (provide ordered `lightSteps`). Otherwise use 'job'",
      "(the DEFAULT): provide a clear `instruction` to an autonomous agent, set",
      "`deep` true for genuinely multi-step work, and optionally list `skills`",
      "from: routing, composio, ssh, scheduling, filesystem, modalities.",
      "",
      "Always set a short human `name` and one-sentence `summary`. Set every field",
      "you are not using to null. Choose 'job' when in doubt.",
    ].join("\n"),
    itemFields: { spec: "string" },
    userTemplate: "Compile this automation request:\n\n{{item.spec}}",
    golden: [
      {
        spec: "when I get an email from alice@acme.com, summarize it and save it to my vfs as a markdown file",
      },
      {
        spec: "every weekday at 9am send me a motivational quote",
      },
    ],
    graderRubric: [
      "Grade an automation compiler output.",
      "Request: {{item.spec}}",
      "triggerKind: {{sample.output_json.triggerKind}}, composioTriggerType:",
      "{{sample.output_json.composioTriggerType}}, composioFilter:",
      "{{sample.output_json.composioFilter}}, cron: {{sample.output_json.cron}},",
      "everyMs: {{sample.output_json.everyMs}}, actionMode:",
      "{{sample.output_json.actionMode}}, instruction:",
      "{{sample.output_json.instruction}}, deep: {{sample.output_json.deep}}",
      "Score 1-7. Good: triggerKind matches the request (email→composio with",
      "GMAIL_NEW_GMAIL_MESSAGE + a from filter; 'every weekday 9am'→schedule cron",
      "'0 9 * * 1-5'); actionMode is 'job' for anything non-trivial with a concrete",
      "instruction; unused fields are null. Penalize a wrong trigger kind, missing",
      "filter, wrong cadence, or 'light' for work that needs reasoning.",
      "4=acceptable, 7=excellent.",
    ].join("\n"),
  },

  // 21. voice transcription (audio — not text-evaluable) ---------------
  {
    id: "voice-transcription",
    name: "Voice transcription",
    blurb: "Whisper transcription of Telegram voice notes. Audio modality — not run through the text Evals API.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "Telegram voice/audio/video_note",
    audio: true,
    structured: false,
    system: "",
    itemFields: {},
    userTemplate: "",
    golden: [],
    graderRubric: "",
  },

  // 22. speech synthesis (audio — not text-evaluable) ------------------
  {
    id: "speech-synthesis",
    name: "Speech synthesis (TTS)",
    blurb: "Text-to-speech for voice replies. Audio modality — not run through the text Evals API.",
    realProvider: "openai",
    evalPurpose: "fast-meta",
    trigger: "send_as_voice tool",
    audio: true,
    structured: false,
    system: "",
    itemFields: {},
    userTemplate: "",
    golden: [],
    graderRubric: "",
  },
];

export function listFlows(): FlowEvalSpec[] {
  return FLOWS;
}

export function getFlow(id: string): FlowEvalSpec | undefined {
  return FLOWS.find((f) => f.id === id);
}

// Build the OpenAI eval's data_source_config.item_schema from a flow's
// itemFields (strict: every field required, additionalProperties false).
export function itemSchemaFor(flow: FlowEvalSpec): JsonSchema {
  const properties: Record<string, JsonSchema> = {};
  for (const [name, t] of Object.entries(flow.itemFields)) {
    properties[name] = { type: t };
  }
  return {
    type: "object",
    additionalProperties: false,
    required: Object.keys(properties),
    properties,
  };
}
