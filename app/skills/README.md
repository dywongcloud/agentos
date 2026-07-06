# NBRI Survey Workflow Skills

Automation specs for NBRI's survey-research delivery lifecycle, derived from
`NBRI_Workflows_1.pdf`. Each phase is one skill the agent can run. These are
**specs**, not yet wired code — they describe the trigger, the automatable
steps, the Composio actions, and the human hand-offs for each phase.

| Phase | Spec | Entry trigger |
|-------|------|---------------|
| 1. Survey Kick-Off | [nbri-kickoff.md](./nbri-kickoff.md) | Deal → "Won" in monday "Active Deals" |
| 2. Pre-Design | [nbri-predesign.md](./nbri-predesign.md) | Client returns documentation packet (email) |
| 3. Design | [nbri-design.md](./nbri-design.md) | Packet sent to Design phase (monday status) |
| 4. Deployment | [nbri-deployment.md](./nbri-deployment.md) | Deployment scheduled / confirmed |
| 5. Reporting | _(referenced in source, not in this document)_ | — |

## Actors

- **Client** — external; receives/sends emails, approves drafts.
- **NBRI Sales** — closes the deal, sets monday stage, assigns RPM.
- **RPM** (Research Project Manager) — the orchestration hub; nearly every
  monday step belongs to RPM.
- **OP** (Organizational Psychologist) — drafts QDB, finalizes QSB. Optional.
- **PSE** (Product Support Engineer) — designs surveys/emails, schedules
  deployment.

## Step classification (used in every spec)

- `AUTO` — agent can do it end-to-end via a Composio action or a notification.
- `DRAFT` — agent prepares content (email/doc) for a human to send/approve.
- `HUMAN` — `MANUAL` / `USER PROCESS TASK` in the source; human judgment, the
  agent can only notify/track.
- `EXTERNAL` — happens in a 3rd-party system (clearpath email, translated.com,
  accounting, PowerPoint).

## Trigger architecture — the monday.com gap

**Verified against this deployment's Composio project (2026-06-05):**

```
op=discover_triggers toolkit=gmail   → 2  (GMAIL_NEW_GMAIL_MESSAGE, GMAIL_EMAIL_SENT_TRIGGER)
op=discover_triggers toolkit=github  → 40
op=discover_triggers toolkit=monday  → 0   ← no native monday triggers
op=discover_triggers keyword=monday  → 0 monday-specific (enum has no MONDAY_* slugs)
```

**Composio exposes NO native monday.com triggers.** Since every NBRI phase is
gated on a monday board-state change, there is no event-driven hook *from
monday through Composio*. Three ways to bridge the gap, in order of preference:

1. **monday native automation → agentOS webhook.** monday.com has its own
   built-in webhook/automation engine ("When status changes to X, send a
   webhook"). Point it at an ingestion endpoint. **Caveat:** the existing
   `/api/claw?op=composio_webhook` handler expects Composio's Svix-style
   payload (`dispatchComposioWebhook` verifies `webhook-signature` and
   version-detects V1/V2/V3). A raw monday payload won't match — this path
   needs a small `op=monday_webhook` adapter that maps monday's
   `{event: {pulseId, columnId, value}}` shape into a `dispatchJob` call.
2. **Poll monday on a cron.** Use the agent's `COMPOSIO_SEARCH_TOOLS('monday
   get items')` → `COMPOSIO_EXECUTE_TOOL` to read board items on a schedule,
   diff stored state, and dispatch when a stage/status column changes. No
   monday config required; higher latency + cost.
3. **Trigger off adjacent toolkits that DO have triggers.** Many NBRI steps
   are "receive/send email" or "meeting" steps that map cleanly onto verified
   triggers:
   - `GMAIL_NEW_GMAIL_MESSAGE` — client returns packet, sends approval, etc.
   - `GMAIL_EMAIL_SENT_TRIGGER` — outbound confirmation/audit.
   - `GOOGLECALENDAR_EVENT_STARTING_SOON_TRIGGER` — intro call, platform training.

## monday.com actions (writes)

`op=discover_actions toolkit=monday` returned 0 in this project (the SDK
`tools.list` shape used by the probe also returned 0 for gmail, so treat the
probe as inconclusive for *actions*, not as proof actions are absent — the
monday toolkit does support actions in Composio generally). At **runtime** the
agent should resolve the exact slug dynamically rather than hard-coding it:

```
COMPOSIO_SEARCH_TOOLS('monday update item status')
COMPOSIO_GET_TOOL_SCHEMAS(<slug>)
COMPOSIO_EXECUTE_TOOL(<slug>, { boardId, itemId, columnValues })
```

Specs below reference monday writes as **capabilities** (e.g. "update status
column → Won") plus the search phrase to resolve them, never an invented slug.

## agentOS primitives these skills use

- `subscribe_to_trigger(slug, config)` — bind a Composio trigger (Gmail/Calendar).
- `/api/claw?op=composio_webhook` — Composio → job dispatch ingestion.
- `dispatchJob({channel, sessionId, senderId, prompt})` — start an agent job.
- `COMPOSIO_SEARCH_TOOLS / GET_TOOL_SCHEMAS / EXECUTE_TOOL` — monday writes.
- cron (see `CronCreate`) — for the polling fallback.
