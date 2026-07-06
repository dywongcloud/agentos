// app/docs/page.tsx
//
// Long-form documentation for DylanClaw. Public route — no auth gate. Renders a
// sticky sidebar TOC + a generous prose column. Originally authored for this
// project; reflects the actual feature surface (slash commands, deep mode,
// triggers, memory, etc.) — not a copy of any upstream docs.

import Link from "next/link";

import "./docs.css";

export const metadata = {
  title: "Docs — DylanClaw",
  description:
    "How to drive the DylanClaw agent: chat surface, slash commands, deep mode, code projects, triggers, memory, and the workflow dashboard.",
};

const TOC = [
  { id: "intro", label: "Intro" },
  { id: "quick-start", label: "Quick start" },
  { id: "talking", label: "Just talking to it" },
  { id: "commands", label: "Slash commands", children: [
    { id: "cmd-job", label: "/job  /deep  /status" },
    { id: "cmd-code", label: "/code  attach  push" },
    { id: "cmd-autopilot", label: "/autopilot  /debug" },
    { id: "cmd-memory", label: "/remember  /memories" },
  ]},
  { id: "deep-mode", label: "Deep mode + reviewer" },
  { id: "code-sessions", label: "Long-running code sessions" },
  { id: "browser", label: "Headless browser" },
  { id: "triggers", label: "Triggers & webhooks" },
  { id: "memory", label: "Memory system" },
  { id: "audit-vs-logs", label: "Audit vs logs" },
  { id: "workflow-ui", label: "Workflow dashboard" },
  { id: "config", label: "Configuration" },
];

function Code({
  cmd,
  comment,
}: {
  cmd: string;
  comment?: string;
}) {
  return (
    <pre className="d-code">
      {comment ? (
        <span className="d-code-comment"># {comment}{"\n"}</span>
      ) : null}
      <span className="d-code-cmd">{cmd}</span>
    </pre>
  );
}

export default function DocsPage() {
  return (
    <main className="d-page">
      <header className="d-topbar">
        <div className="d-topbar-inner">
          <Link href="/home" className="d-brand">
            <span className="d-brand-mark">D</span>
            DylanClaw <span className="d-brand-tag">docs</span>
          </Link>
          <nav className="d-top-links">
            <Link href="/home" className="d-top-link">Home</Link>
            <Link href="/ui" className="d-top-link">Dashboard</Link>
            <a
              href="https://github.com/vercel-labs/workflow"
              className="d-top-link"
              target="_blank"
              rel="noreferrer"
            >
              Workflow SDK ↗
            </a>
          </nav>
        </div>
      </header>

      <div className="d-layout">
        <aside className="d-toc">
          <h2 className="d-toc-title">On this page</h2>
          <ul className="d-toc-list">
            {TOC.map((item) => (
              <li key={item.id}>
                <a href={`#${item.id}`}>{item.label}</a>
                {item.children?.map((c) => (
                  <a key={c.id} href={`#${c.id}`} className="d-toc-sub">
                    {c.label}
                  </a>
                ))}
              </li>
            ))}
          </ul>
        </aside>

        <article className="d-main">
          <div className="d-hero">
            <div className="d-eyebrow">documentation</div>
            <h1 className="d-h1">Drive the agent.</h1>
            <p className="d-lede">
              DylanClaw is a Telegram-first agent backed by Vercel&apos;s Workflow
              DevKit. This page covers how to talk to it from chat, what every
              slash command does, and how the moving pieces — deep mode, code
              sessions, browser, triggers, memory — fit together.
            </p>
          </div>

          {/* ── intro ───────────────────────────────────────────────── */}
          <section id="intro" className="d-section">
            <h2 className="d-h2">Intro</h2>
            <p className="d-p">
              The agent runs as a serverless Vercel app. Conversational turns
              come in over Telegram (with WhatsApp and SMS available as
              alternates) and flow through a durable workflow. Long-running
              work — research, coding, browsing, deep multi-pass synthesis —
              is dispatched as a separate workflow run you can inspect and
              follow up on later.
            </p>
            <p className="d-p">
              Every meaningful state change (integration connected, trigger
              subscribed, settings flipped) is recorded in a dedicated audit
              log. Every operational event (a job ran, a tool fired, a
              webhook delivered) lives in a separate event stream. The two
              are intentionally distinct so &quot;what changed&quot; never gets
              drowned out by &quot;what happened.&quot;
            </p>
            <div className="d-callout">
              <span className="d-callout-icon">→</span>
              <div className="d-callout-text">
                The dashboard you&apos;re looking for is at{" "}
                <code>/ui</code>. The literal upstream Vercel Workflow
                observability dashboard lives at <code>/</code>, behind the
                same session gate.
              </div>
            </div>
          </section>

          {/* ── quick start ─────────────────────────────────────────── */}
          <section id="quick-start" className="d-section">
            <h2 className="d-h2">Quick start</h2>
            <p className="d-p">
              Pair your Telegram chat to your tenant, then start talking. The
              agent will use your connected Composio integrations
              transparently — no slash command required.
            </p>
            <h3 className="d-h3">1. Pair Telegram</h3>
            <Code
              cmd="/pair <code-from-/ui-settings>"
              comment="paste the 6-digit code from the Settings tab"
            />
            <h3 className="d-h3">2. Try something</h3>
            <Code cmd="What did I work on yesterday?" />
            <Code cmd="Find me the cheapest flight from SFO to Tokyo next month" />
            <Code cmd="/deep what's the unit economics of Cloudflare Workers vs Lambda" />
            <h3 className="d-h3">3. Open the dashboard</h3>
            <p className="d-p">
              Sign in to <code>/ui</code> with the password set as{" "}
              <code>ADMIN_UI_PASSWORD</code>. You&apos;ll see the audit log,
              live event stream, workflow runs, and per-tenant integrations.
            </p>
          </section>

          {/* ── talking ─────────────────────────────────────────────── */}
          <section id="talking" className="d-section">
            <h2 className="d-h2">Just talking to it</h2>
            <p className="d-p">
              Most things you&apos;d want to do work as plain English. The
              agent classifies depth automatically — short questions get a
              fast response on Gemini Flash; meaty research questions get
              routed to deep mode without you typing <code>/deep</code>.
            </p>
            <p className="d-p">
              Tool access is dynamic via Composio. If you have Gmail
              connected and you say{" "}
              <em>&quot;summarize the email from Sarah today&quot;</em>, the
              agent discovers and invokes the right Gmail tool with no
              hand-holding. If you ask to be notified about something, it
              walks the natural-language subscribe flow — discover trigger →
              check connection → subscribe.
            </p>
            <p className="d-p">
              You can also send voice notes — they&apos;re transcribed via
              Whisper and processed like any other turn. Ask for a spoken
              reply and the bot will send one back via OpenAI TTS.
            </p>
          </section>

          {/* ── commands ────────────────────────────────────────────── */}
          <section id="commands" className="d-section">
            <h2 className="d-h2">Slash commands</h2>
            <p className="d-p">
              These are the explicit handles when you want them. They&apos;re
              not required — every action below has a natural-language path —
              but they&apos;re faster when you know what you want.
            </p>

            <div className="d-table-wrap">
              <table className="d-table">
                <thead>
                  <tr>
                    <th>Command</th>
                    <th>Effect</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td className="d-cmd-cell">/job &lt;prompt&gt;</td>
                    <td>Dispatch any task as a tracked run. Returns an ID; <code>/status &lt;id&gt;</code> peeks in.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/deep &lt;prompt&gt;</td>
                    <td>Force pro-extended deep research mode. Reviewer + escalation, ~$8–20 budget.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/extended &lt;prompt&gt;</td>
                    <td>Alias for /deep.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/code &lt;task&gt;</td>
                    <td>Start a long-running Claude Code (or OpenCode) project in a Vercel Sandbox.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/code attach &lt;id&gt; &lt;follow-up&gt;</td>
                    <td>Continue an existing project (resumes session history).</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/code push &lt;id&gt; &lt;repo&gt;</td>
                    <td>Commit + push the project workdir to GitHub via Composio.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/status &lt;jobId&gt;</td>
                    <td>Status + last thoughts of any tracked run.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/ask &lt;jobId&gt; &lt;q&gt;</td>
                    <td>Read-only side-channel question about a running job.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/autopilot on|off</td>
                    <td>Opt in/out of the 1-minute heartbeat that pings you when something matters.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/debug on|off</td>
                    <td>Toggle verbose streaming (Thinking…, tool calls) vs the quiet final-reply mode.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/remember &lt;text&gt;</td>
                    <td>Save a fact to long-term memory. <code>/remember chat</code> summarizes the whole session.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/memories</td>
                    <td>List recent memories. <code>/memforget &lt;id&gt;</code> deletes one.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/subscribe SLUG</td>
                    <td>Power-user trigger subscribe. The natural-language flow is preferred.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/triggers</td>
                    <td>List your active Composio trigger subscriptions.</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/logins</td>
                    <td>List saved browser sessions (cookies captured by the headless browser).</td>
                  </tr>
                  <tr>
                    <td className="d-cmd-cell">/stop  /start</td>
                    <td>Hard pause/resume — bot won&apos;t respond to anything between them.</td>
                  </tr>
                </tbody>
              </table>
            </div>

            <h3 className="d-h3" id="cmd-job">/job, /deep, /status</h3>
            <p className="d-p">
              A <code>/job</code> dispatch is fire-and-forget. You get a job
              ID back immediately, the bot keeps responding to other messages
              while it runs, and a final answer drops in chat when it&apos;s
              done. <code>/deep</code> is the same but forces the orchestrator
              into pro-extended mode (more iterations, reviewer sub-agent,
              escalation to gpt-5.4-pro + Gemini cross-review).
            </p>
            <Code
              cmd="/deep compare the unit economics of Cloudflare Workers vs AWS Lambda"
              comment="multi-pass research; final answer cites sources inline"
            />
            <Code cmd="/status j_573511d8" comment="state, cost so far, recent thoughts" />
            <Code cmd="/ask j_573511d8 are you on the open-source numbers?" comment="read-only check-in" />

            <h3 className="d-h3" id="cmd-code">/code, attach, push</h3>
            <p className="d-p">
              <code>/code</code> opens a long-running coding session in a
              persistent Vercel Sandbox running either Claude Code or
              OpenCode. The workdir lives on the VFS at{" "}
              <code>/workspace/claude_code/projects/&lt;projectId&gt;</code>,
              the auth state is snapshotted to Redis between turns, and the
              project can be materialized to GitHub when you&apos;re ready.
            </p>
            <Code cmd="/code build a fastify hello-world with a /health route" />
            <Code
              cmd="/code attach p_a1b2c3 now add a Dockerfile and a test"
              comment="claude --continue resumes the session"
            />
            <Code cmd="/code push p_a1b2c3 https://github.com/me/myproject" />

            <h3 className="d-h3" id="cmd-autopilot">/autopilot, /debug</h3>
            <p className="d-p">
              <code>/autopilot on</code> opts your tenant into the
              proactive heartbeat. Every minute a cheap classifier (Gemini
              Flash) peeks at recent activity — completed jobs, stuck
              projects, idle reminders — and decides whether to message you.
              Bias is toward silence; it only reaches out when there&apos;s a
              real hook. Default cooldown is 15 min.
            </p>
            <p className="d-p">
              <code>/debug on</code> swaps the default quiet UX for
              the verbose streaming experience: typewriter effect, &quot;Thinking…&quot;
              placeholder, live tool-call status. Per-tenant, persists across
              sessions.
            </p>

            <h3 className="d-h3" id="cmd-memory">/remember, /memories</h3>
            <p className="d-p">
              The memory store is per-tenant, Redis-backed, and addressable
              by kind (<code>directory</code>, <code>command</code>,{" "}
              <code>preference</code>, <code>project</code>,{" "}
              <code>favorite_app</code>, <code>chat_summary</code>, …). Top
              relevant memories are auto-injected into every chat turn&apos;s
              system prompt.
            </p>
            <Code cmd="/remember I deploy from main, never from feature branches" />
            <Code cmd="/remember chat" comment="summarizes this session as a memory" />
          </section>

          {/* ── deep mode ───────────────────────────────────────────── */}
          <section id="deep-mode" className="d-section">
            <h2 className="d-h2">Deep mode + reviewer</h2>
            <p className="d-p">
              Deep mode runs a bounded orchestrator loop on gpt-5.4 (and
              gpt-5.3-codex on revise/near-end), with per-attempt iteration
              caps so the orchestrator can&apos;t research indefinitely on a
              single attempt. After each attempt, a correctness critic
              (modality-specific rubric on o3) gates the result.
            </p>
            <p className="d-p">
              If correctness passes, the <strong className="d-strong">depth reviewer</strong>{" "}
              runs. It scores the draft on four axes — insight, data
              density, coverage, rigor — and a Gemini 3.1 Pro cross-reviewer
              adds concrete missing data points. Verdicts:
            </p>
            <ul className="d-ul">
              <li className="d-li"><strong className="d-strong">accept</strong> — ship it.</li>
              <li className="d-li"><strong className="d-strong">more_passes</strong> — loop back with the gaps as verifier notes.</li>
              <li className="d-li">
                <strong className="d-strong">escalate</strong> — flip the pro-tier flag, bump the orchestrator + subtasks to gpt-5.4-pro at forced high reasoning effort, and re-attack.
              </li>
            </ul>
            <p className="d-p">
              The escalated path gets a larger dollar budget
              (<code>BUDGET_USD_PER_DEEP_JOB_ESCALATED</code>, default $20)
              vs. the base ($8). Depth pass count is capped at 3, and the
              machine&apos;s revise budget is capped at 6 — both belt-and-
              suspenders bounds beneath the cost gate.
            </p>
          </section>

          {/* ── code sessions ───────────────────────────────────────── */}
          <section id="code-sessions" className="d-section">
            <h2 className="d-h2">Long-running code sessions</h2>
            <p className="d-p">
              <code>/code</code> creates a code project and fires it as a
              durable workflow run. Engine selection: Claude Code (when{" "}
              <code>ANTHROPIC_API_KEY</code> is set) → OpenCode fallback
              (uses <code>OPENAI_API_KEY</code> and the configured coding
              model). The sandbox is the shared <code>claw-browser</code> Vercel
              Sandbox, so cold-starts only happen once across browser + code.
            </p>
            <p className="d-p">
              Project workdirs are stable across turns:{" "}
              <code>
                /tmp/claw-browser/cc-workdirs/&lt;tenant&gt;/projects/&lt;projectId&gt;/
              </code>
              . Each turn runs <code>claude --continue</code> (or
              <code> opencode run --continue</code>) so session history
              resumes. The current state is also materialized to the per-
              tenant VFS at <code>/workspace/claude_code/projects/&lt;id&gt;/</code>{" "}
              — manifest, per-turn output, task history — so the project
              survives a cold sandbox.
            </p>
          </section>

          {/* ── browser ─────────────────────────────────────────────── */}
          <section id="browser" className="d-section">
            <h2 className="d-h2">Headless browser</h2>
            <p className="d-p">
              Real Chromium + Playwright in a Vercel Sandbox, driven by
              OpenAI&apos;s computer-use-preview model in a screenshot →
              action loop. Per-session residential proxy from Webshare
              keeps the IP US-residential and sticky.
            </p>
            <p className="d-p">
              Before the driver sees your prompt, a{" "}
              <strong className="d-strong">Gemini 3.1 Pro side-car</strong>{" "}
              expands it into a multi-step navigation plan — best start URL,
              ordered actions, pitfalls (cookie walls, logins, pagination),
              and exactly what to extract. The plan becomes the driver&apos;s
              goal, so multi-page tasks navigate far more reliably.
            </p>
            <p className="d-p">
              Logged-in sessions stick. When the bot signs into a site, the
              cookies are captured into the per-tenant{" "}
              <code>browserAuthStore</code> (AES-GCM encrypted). Future
              browses for the same tenant load those cookies into Chromium
              automatically.
            </p>
          </section>

          {/* ── triggers ────────────────────────────────────────────── */}
          <section id="triggers" className="d-section">
            <h2 className="d-h2">Triggers &amp; webhooks</h2>
            <p className="d-p">
              Triggers are subscribed by chatting — &quot;let me know when I get
              a new email&quot; — and the agent walks the discover → check-
              connection → subscribe flow itself. Events from Composio
              arrive at <code>/api/claw?op=composio_webhook</code> (the
              webhook subscription is registered automatically on first
              setup) and are version-detected (V1/V2/V3) before delivery to
              your chat session.
            </p>
            <p className="d-p">
              Signing: the receiver uses Svix-style{" "}
              <code>webhook-id</code> + <code>webhook-timestamp</code> +{" "}
              <code>webhook-signature</code> headers, verified via the
              Composio SDK. By default verification is lenient (a signature
              quirk won&apos;t silently drop a real email);
              <code>COMPOSIO_WEBHOOK_STRICT=true</code> rejects on failure.
            </p>
          </section>

          {/* ── memory ──────────────────────────────────────────────── */}
          <section id="memory" className="d-section">
            <h2 className="d-h2">Memory system</h2>
            <p className="d-p">
              Memories are per-tenant Redis entries with kind, title,
              summary, labels, and timestamps. They&apos;re queryable
              deterministically (by kind, by tag, by keyword) and the top
              relevant entries are auto-injected into every chat turn&apos;s
              system prompt.
            </p>
            <p className="d-p">
              <code>/remember chat</code> distils the current session into
              both a paragraph summary and atomic facts — useful when the
              bot has been bouncing through a long thread. Enrichment
              (writing a tight summary + tags from raw text) is delegated to
              gpt-4o.
            </p>
            <p className="d-p">
              The memory kinds in use: <code>preference</code>,{" "}
              <code>directory</code>, <code>command</code>,{" "}
              <code>favorite_app</code>, <code>chat_summary</code>,{" "}
              <code>project</code>, <code>reminder</code>,{" "}
              <code>system</code>, plus a few more.
            </p>
          </section>

          {/* ── audit vs logs ────────────────────────────────────────── */}
          <section id="audit-vs-logs" className="d-section">
            <h2 className="d-h2">Audit log vs event stream</h2>
            <p className="d-p">
              These are intentionally separate. The{" "}
              <strong className="d-strong">audit log</strong> (Activity tab in{" "}
              <code>/ui</code>) only records state changes — Composio
              integration connect / disconnect / expire, trigger sub/unsub,
              settings flips (<code>/debug</code>, <code>/autopilot</code>).
              A drift detector compares Composio&apos;s live connected-
              account list against a snapshot on every audit-panel poll, so
              an expired connection surfaces here without waiting for the
              next cron.
            </p>
            <p className="d-p">
              The <strong className="d-strong">event stream</strong> (Logs tab) is
              everything else — jobs dispatched, tool calls, webhook
              deliveries, code-project turns, agent reactions. It&apos;s
              merged across <code>activityLog</code>, the Composio webhook
              ring, <code>jobStore</code>, and <code>codeProjectStore</code>, and
              polls every 8s.
            </p>
            <p className="d-p">
              Tl;dr — <em>Activity = what changed</em>,{" "}
              <em>Logs = what&apos;s happening right now</em>.
            </p>
          </section>

          {/* ── workflow dashboard ──────────────────────────────────── */}
          <section id="workflow-ui" className="d-section">
            <h2 className="d-h2">Workflow dashboard</h2>
            <p className="d-p">
              The literal upstream Vercel Workflow DevKit dashboard
              (<code>@workflow/web</code>) is mounted at the app root. Same
              session cookie as <code>/ui</code> gates access; unauthenticated
              HTML requests redirect to <code>/ui/login</code> and
              unauthenticated data calls (<code>/api/rpc</code>,{" "}
              <code>/__manifest</code>, <code>/api/stream/*</code>) return
              401.
            </p>
            <p className="d-p">
              Mount details live in <code>app/wf-app/[[...slug]]/route.ts</code>{" "}
              + the rewrites in <code>next.config.ts</code>. The express app
              from <code>@workflow/web</code>&apos;s built server bundle is
              loaded at runtime and bridged through a small Node↔Web adapter
              (<code>app/lib/expressBridge.ts</code>), so binary CBOR
              responses round-trip without corruption.
            </p>
          </section>

          {/* ── config ──────────────────────────────────────────────── */}
          <section id="config" className="d-section">
            <h2 className="d-h2">Configuration</h2>
            <p className="d-p">All env vars are set in Vercel. Key ones:</p>

            <div className="d-table-wrap">
              <table className="d-table">
                <thead>
                  <tr>
                    <th>Env</th>
                    <th>Purpose</th>
                  </tr>
                </thead>
                <tbody>
                  <tr><td className="d-flag-cell">ADMIN_UI_PASSWORD</td><td>The /ui sign-in password (HMAC-signed cookie thereafter).</td></tr>
                  <tr><td className="d-flag-cell">TELEGRAM_BOT_TOKEN</td><td>Bot identity for inbound + outbound Telegram messages.</td></tr>
                  <tr><td className="d-flag-cell">COMPOSIO_API_KEY</td><td>Tool access + trigger subscriptions.</td></tr>
                  <tr><td className="d-flag-cell">COMPOSIO_WEBHOOK_SECRET</td><td>Svix-style webhook signing secret.</td></tr>
                  <tr><td className="d-flag-cell">OPENAI_API_KEY</td><td>GPT-5.x family, o3/o3-pro reasoning, computer-use-preview, Whisper, TTS.</td></tr>
                  <tr><td className="d-flag-cell">ANTHROPIC_API_KEY</td><td>Claude Code sessions.</td></tr>
                  <tr><td className="d-flag-cell">GOOGLE_GENERATIVE_AI_API_KEY</td><td>Gemini 3.5 Flash (chat) + Gemini 3.1 Pro (browser brain, depth reviewer cross-check).</td></tr>
                  <tr><td className="d-flag-cell">UPSTASH_REDIS_REST_URL / _TOKEN</td><td>All persistent state.</td></tr>
                  <tr><td className="d-flag-cell">BUDGET_USD_PER_DEEP_JOB</td><td>Cap per deep job. Default $8.</td></tr>
                  <tr><td className="d-flag-cell">BUDGET_USD_PER_DEEP_JOB_ESCALATED</td><td>Cap after the reviewer escalates. Default $20.</td></tr>
                  <tr><td className="d-flag-cell">DEEP_MAX_DEPTH_PASSES</td><td>How many reviewer-driven loops max. Default 3.</td></tr>
                  <tr><td className="d-flag-cell">DEEP_PER_ATTEMPT_ITERS</td><td>Per-attempt orchestrator iteration cap. Default 7.</td></tr>
                  <tr><td className="d-flag-cell">AUTOPILOT_PROACTIVE_COOLDOWN_MIN</td><td>Minutes between proactive pings. Default 15.</td></tr>
                  <tr><td className="d-flag-cell">CHAT_MODEL_NAME</td><td>Override the chat base. Default gemini-3.5-flash.</td></tr>
                  <tr><td className="d-flag-cell">SMART_MODEL_NAME / CODING_MODEL_NAME / REASONING_MODEL_NAME</td><td>Override the per-purpose model picks.</td></tr>
                  <tr><td className="d-flag-cell">TELEGRAM_REACTIONS_ENABLED</td><td>Set to <code>false</code> to silence emoji reactions.</td></tr>
                </tbody>
              </table>
            </div>

            <p className="d-p" style={{ marginTop: 28 }}>
              That&apos;s the whole map. Drop into <code>/ui</code> and start
              poking — the audit log will tell you what you changed and the
              event stream will tell you what the agent did about it.
            </p>
          </section>
        </article>
      </div>
    </main>
  );
}
