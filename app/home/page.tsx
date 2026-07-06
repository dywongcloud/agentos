// app/home/page.tsx
//
// Marketing / landing page for DylanClaw. Public (no auth gate) so prospective
// users can read what the bot does without signing in. Uses the same theme
// tokens as /ui so light/dark mode both render correctly.

import Link from "next/link";

import "./home.css";

export const metadata = {
  title: "DylanClaw — your personal agentOS",
  description:
    "A persistent, multi-channel agent that runs jobs, browses the web, codes, and remembers what matters. Built on Vercel Workflow.",
};

const FEATURES = [
  {
    title: "Chat-driven agent",
    body: "Telegram, WhatsApp, SMS. Composio tools wired in. Streaming responses, voice in & out, real reactions when something lands.",
    icon: "✦",
  },
  {
    title: "Deep research jobs",
    body: "Multi-pass research orchestrator with a reviewer sub-agent that scores insight and escalates to gpt-5.4-pro + Gemini 3.1 Pro when depth is thin.",
    icon: "◇",
  },
  {
    title: "Code projects",
    body: "Long-running Claude Code or OpenCode sessions in a Vercel Sandbox. Project state persists on the VFS; push to GitHub when ready.",
    icon: "▣",
  },
  {
    title: "Real headless browser",
    body: "Chromium + Playwright + Webshare residential proxy in a sandbox. Gemini 3.1 Pro pre-plans multi-step navigation before the driver clicks.",
    icon: "⌖",
  },
  {
    title: "Triggers, by chat",
    body: "Say \"let me know when I get a new email.\" The agent discovers the Composio trigger, hands you the OAuth link if needed, subscribes you.",
    icon: "●",
  },
  {
    title: "Memory that lasts",
    body: "Per-tenant facts, preferences, directories, favorite apps. Deterministic recall. gpt-4o enriches summaries; chat history compacts on demand.",
    icon: "✿",
  },
  {
    title: "Workflow observability",
    body: "The literal upstream Vercel Workflow dashboard runs at /, gated behind your session. Every run, step, and event — first-party.",
    icon: "◆",
  },
  {
    title: "Audit log",
    body: "Every integration connect / disconnect / expire and every settings flip is recorded separately from the event stream. Drift-detected.",
    icon: "⌘",
  },
  {
    title: "Proactive autopilot",
    body: "A 1-minute heartbeat decides when to reach out — only when there's a real hook. Cheap (Gemini Flash), human, never spammy.",
    icon: "↗",
  },
];

const COMMANDS = [
  { cmd: "/job", tail: "dispatch any task" },
  { cmd: "/deep", tail: "pro-extended research" },
  { cmd: "/code", tail: "start a coding session" },
  { cmd: "/status <id>", tail: "peek into a run" },
  { cmd: "/autopilot on", tail: "let it ping you" },
  { cmd: "/remember", tail: "save preferences" },
  { cmd: "/debug on", tail: "stream tool calls" },
];

export default function HomePage() {
  return (
    <main className="h-page">
      <div className="h-shell">
        <nav className="h-nav">
          <Link href="/home" className="h-brand">
            <span className="h-brand-mark">D</span>
            DylanClaw
          </Link>
          <div className="h-nav-links">
            <Link href="/docs" className="h-nav-link">
              Docs
            </Link>
            <Link href="/ui" className="h-nav-link">
              Dashboard
            </Link>
            <a
              href="https://github.com/vercel-labs/workflow"
              className="h-nav-link"
              target="_blank"
              rel="noreferrer"
            >
              Workflow SDK
            </a>
            <Link href="/ui/login" className="h-nav-cta">
              Sign in
            </Link>
          </div>
        </nav>

        <section className="h-hero">
          <span className="h-eyebrow">
            <span className="h-eyebrow-dot" />
            agent · runtime · live
          </span>
          <h1 className="h-title">
            Your personal agentOS,
            <br />
            actually running.
          </h1>
          <p className="h-sub">
            A persistent multi-channel agent built on Vercel&apos;s Workflow
            DevKit. Composio for tools, Claude / GPT-5 / Gemini for thinking,
            durable workflows for everything that takes more than a turn.
            One dashboard, every signal.
          </p>
          <div className="h-cta-row">
            <Link href="/ui" className="h-cta h-cta--primary">
              Launch dashboard
              <span className="h-cta-arrow">→</span>
            </Link>
            <Link href="/docs" className="h-cta h-cta--ghost">
              Read the docs
            </Link>
          </div>
        </section>

        <section className="h-section">
          <div className="h-section-head">
            <span className="h-section-tag">capabilities</span>
            <h2 className="h-section-title">
              Built for tasks that don&apos;t fit a single LLM call.
            </h2>
            <p className="h-section-sub">
              Every primitive — runs, steps, browser, sandboxes, memory,
              triggers — is durable, observable, and reachable from chat.
            </p>
          </div>

          <div className="h-grid">
            {FEATURES.map((f) => (
              <article key={f.title} className="h-card">
                <div className="h-card-icon">{f.icon}</div>
                <h3 className="h-card-title">{f.title}</h3>
                <p className="h-card-body">{f.body}</p>
              </article>
            ))}
          </div>
        </section>

        <section className="h-section">
          <div className="h-section-head">
            <span className="h-section-tag">try it in chat</span>
            <h2 className="h-section-title">A short, honest command surface.</h2>
            <p className="h-section-sub">
              Most things you&apos;d expect work through plain conversation.
              These slash commands are the explicit handles when you want them.
            </p>
          </div>
          <div className="h-rail">
            <div className="h-rail-head">
              <div>
                <h3 className="h-rail-title">Slash commands</h3>
                <div className="h-rail-sub">
                  See <Link href="/docs" style={{ color: "var(--foreground)" }}>the full reference</Link>{" "}
                  for every flag, plus the natural-language paths.
                </div>
              </div>
            </div>
            <div className="h-rail-list">
              {COMMANDS.map((c) => (
                <span key={c.cmd} className="h-pill">
                  <code>{c.cmd}</code>
                  <span className="h-pill-tail">{c.tail}</span>
                </span>
              ))}
            </div>
          </div>
        </section>

        <footer className="h-footer">
          <div>DylanClaw — built on Vercel Workflow DevKit</div>
          <div className="h-footer-links">
            <Link href="/ui" className="h-footer-link">Dashboard</Link>
            <Link href="/docs" className="h-footer-link">Docs</Link>
            <a
              href="https://github.com/vercel-labs/workflow"
              className="h-footer-link"
              target="_blank"
              rel="noreferrer"
            >
              Workflow source
            </a>
          </div>
        </footer>
      </div>
    </main>
  );
}
