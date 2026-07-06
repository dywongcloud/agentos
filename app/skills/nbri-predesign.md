# Skill: NBRI Survey Pre-Design

Automates the documentation-packet intake, the QDB (Question Database) draft +
client-approval loop, and filing the finalized QDB into the Design phase.

## Entry trigger

- **Primary:** Client returns the completed NBRI documentation packet —
  arrives by email → `GMAIL_NEW_GMAIL_MESSAGE` (with attachment). This is a
  natural Composio trigger and the recommended entry point for this phase.
- **Secondary:** monday item enters "Pre-Design" status (monday native
  webhook / poll, per [README](./README.md#trigger-architecture--the-mondaycom-gap)).

## Steps

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 1 | Client | Receive deliverables, complete + send doc packet | EXTERNAL | trigger on inbound email |
| 2 | RPM | Receive documentation package (monday) | AUTO | log receipt on the monday item; attach files |
| 3 | RPM | Review + file packet | HUMAN (`USER PROCESS TASK`) | agent pre-checks completeness (checklist) and flags gaps |

### Decision: packet complete?

- **Yes:** send package to begin Design phase (monday status → Design);
  **notify PSE** "package sent — to Design phase".
- **No:** draft + send "request additional information" email to client; loop
  back to step 1 on the client's reply.

### OP sub-process (uses OP's own processes)

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 4 | OP | Review ICD, call notes, previous survey | HUMAN | agent assembles these into one packet for OP |
| 5 | OP | Create draft QDB | HUMAN | — |
| 6 | OP | Conduct QDB meeting (client provides more info) | HUMAN | schedule via Calendar; arm starting-soon trigger |
| 7 | OP | Finalize draft with new info, send draft | DRAFT | agent routes the draft to RPM |

### Client approval loop

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 8 | RPM | Receive draft from OP (monday) | AUTO | log + attach to item |
| 9 | RPM | Send draft to client (monday) | DRAFT/AUTO | draft cover email + send |
| 10 | Client | Review + approve draft | EXTERNAL | watch reply |

- **Approved:** client sends approval email → RPM logs approval, sends
  completed draft to OP.
- **Not approved:** re-run steps 4–9 to re-establish proper documents.

### Finalize

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 11 | OP | Receive final QDB, finalize QSB, send (EMAIL) | HUMAN/DRAFT | `USER PROCESS` (OP); agent drafts the send email |
| 12 | RPM | Receive QDB, file + send (→ Design phase) | AUTO | monday status → Design; notify |
| 13 | Client | Receive QDB | EXTERNAL | — |

### Exit

- QDB filed, monday status → Design. Hand off to **Design**.

## Notifications

- Telegram ping to RPM on: packet received, completeness gaps found, draft
  sent to client, client approval received, QDB filed to Design.
