# Skill: NBRI Design

Automates the survey/email design proof + approval loops, the optional
translation sub-process, and the deployment-scheduling handshake into the
Deployment phase. This is the most loop-heavy phase — multiple client
approval cycles, each of which the agent tracks and shepherds.

## Entry trigger

- **Primary:** monday item status → **"Design"** (set at the end of
  Pre-Design). monday native webhook / poll per
  [README](./README.md#trigger-architecture--the-mondaycom-gap).
- **Adjacent:** `GMAIL_NEW_GMAIL_MESSAGE` to catch each client
  approval / change-request reply during the loops below.

## Steps

### PSE design + first proof

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 1 | PSE | Start survey design + communication | HUMAN (`USER PROCESS`) | — |
| 2 | PSE | Send email proofs + survey login creds | DRAFT | agent packages proofs/creds into the send |
| 3 | RPM | Receive proofs+creds, forward to client | AUTO | log on monday; send to client |

### Client proof approval loop

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 4 | Client | Receive proofs + creds | EXTERNAL | trigger on reply |
| — | | **Approved?** | branch | |
| 4a | Client | (Y) Send approval | EXTERNAL | detect approval → jump to translations decision |
| 4b | Client | (N) Send change request | EXTERNAL | route to RPM → PSE |
| 5 | RPM | Receive + review changes (monday), send for updates | AUTO | log + route to PSE |
| 6 | PSE | Make requested changes | HUMAN (`USER PROCESS`) | — |
| 7 | PSE | Send change updates → RPM → client | DRAFT | loop back to step 4 |
| 8 | RPM | On client approval, send approval (monday) | AUTO | record approval on item |

### Decision: translations required?

- **Yes:** send survey text to `translated.com` (EXTERNAL); on English text
  back, PSE designs translated surveys/emails (`USER PROCESS`), send test
  emails. Loop changes as needed.
- **No:** schedule test emails directly.

### Test-email approval loop

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 9 | PSE | Send new emails/proofs/surveys | DRAFT | — |
| 10 | RPM | Inform/review test emails, send for client approval | AUTO | log + route |
| 11 | Client | Receive test emails etc. — **approve?** | EXTERNAL | Y→approval; N→changes back to RPM |
| 12 | RPM | Receive approval, send deployment schedule, notify | AUTO/DRAFT | draft schedule email |

### Deployment-schedule approval loop

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 13 | Client | Receive deployment schedule — **approve?** | EXTERNAL | Y→approval; N→PSE change loop |
| 14 | RPM | Receive + send approval (monday) | AUTO | record |
| 15 | PSE | Schedule deployment (`USER PROCESS`), confirm, begin pre-deployment | HUMAN | agent tracks the confirmation |
| 16 | RPM | Receive "deployment scheduled" confirmation, email schedule confirmation | AUTO/DRAFT | monday status → Deployment; notify |

### Exit

- Deployment scheduled + confirmed, monday status → Deployment. Hand off to
  **Deployment**.

## Loop-tracking note

Design has **4 nested approval loops** (proof, translation, test-email,
schedule). The skill should keep a small per-item state record (which loop,
which revision #) so the agent can answer "where is project X stuck?" and so a
re-entry from a Gmail reply resumes the correct loop instead of restarting.

## Notifications

- Telegram ping to RPM on: each client approval/change-request, translation
  round-trip, deployment scheduled.
