# Skill: NBRI Survey Kick-Off

Automates the orchestration + notification layer of NBRI's Survey Kick-Off
phase: from a closed-won deal through retainer invoicing, account setup, the
intro call, and the optional OP assignment, into deliverables.

## Entry trigger

- **Primary:** monday "Active Deals" item status → **"Won"**.
  - No Composio trigger exists for monday → use a monday native automation
    ("When status changes to Won, send webhook") → `op=monday_webhook`
    adapter (see [README](./README.md#trigger-architecture--the-mondaycom-gap)),
    OR poll the Active Deals board on a cron and diff the status column.
- **Adjacent assist:** `GMAIL_NEW_GMAIL_MESSAGE` to catch the client's signed
  proposal file and the paid-invoice confirmation arriving by email.

## Inputs / context

- `boardId` (Active Deals), `itemId` (the deal), client contact email,
  assigned RPM (set manually by Sales).

## Steps

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 1 | Client | Approve proposal, send signed file | EXTERNAL | watch via `GMAIL_NEW_GMAIL_MESSAGE` (attachment) |
| 2 | Sales | Review proposal for "won" deal | HUMAN | notify only |
| 3 | Sales | monday: change stage → "Won" in Active Deals | AUTO | `COMPOSIO_SEARCH_TOOLS('monday update item status')` → set status column |
| 4 | Sales | Assign RPM | HUMAN (`MANUAL`) | agent posts a monday update tagging the chosen RPM; assignment decision stays human |
| 5 | Sales | Send retainer invoice (3rd-party accounting) | EXTERNAL/DRAFT | agent drafts the invoice-request email; sending is external accounting |
| 6 | Client | Receive + pay initial invoice | EXTERNAL | watch for payment-confirmation email |
| 7 | RPM | New-account admin tasks | HUMAN (`MANUAL`) | agent creates the monday subtask checklist for the new account |
| 8 | RPM | Tasks per "won" deal notif email | HUMAN (`MANUAL`) | agent parses the notif email → creates monday subitems |
| 9 | RPM | Email request for intro call | DRAFT | agent drafts + (optionally) sends the intro-call request email |
| 10 | Client | Review request, confirm meeting | EXTERNAL | watch reply via Gmail |
| 11 | RPM | Schedule meeting | AUTO | create Google Calendar event; arm `GOOGLECALENDAR_EVENT_STARTING_SOON_TRIGGER` |
| 12 | RPM | Conduct intro call (PowerPoint, scripted) | HUMAN | agent attaches the scripted deck + call-notes template to the monday item |

### Decision: OP (Organizational Psychologist) required?

- **Yes:** monday — assign OP (capability: update people/assignee column),
  send email to OP, move item to "Deliverables". `USER PROCESS TASK` for the
  human decision; the assignment write + email draft are AUTO/DRAFT.
- **No:** move straight to "Deliverables".

### Exit

- Client receives deliverables. If OP assigned, OP receives the project-
  engagement request. Hand off to **Pre-Design**.

## What this skill does NOT do

- Decide *which* RPM/OP to assign (human).
- Send money / issue invoices (external accounting system).
- Conduct the intro call (human, scripted).

## Notifications

- Telegram ping to the RPM session on: status→Won, invoice paid, intro call
  confirmed, OP assigned.
