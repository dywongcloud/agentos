# Skill: NBRI Deployment

Automates survey deployment (online via clearpath, or telephone sub-process),
fielding/monitoring, platform-training coordination, and the close-survey
approval handshake into the Reporting phase.

## Entry trigger

- **Primary:** monday item status → **"Deployment"** (set at the end of
  Design). monday native webhook / poll per
  [README](./README.md#trigger-architecture--the-mondaycom-gap).
- **Adjacent:**
  - `GOOGLECALENDAR_EVENT_STARTING_SOON_TRIGGER` — platform-training sessions.
  - `GMAIL_NEW_GMAIL_MESSAGE` — client date confirmations + close approval.

## Steps

### PSE deployment (mode branch)

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 1 | PSE | Schedule deployment emails | HUMAN (`USER PROCESS`) | agent preps the schedule |
| 2 | PSE | **(Online)** Send invitation emails (clearpath) | EXTERNAL (automated) | clearpath handles send; agent records send time |
| 3 | PSE | **(Online)** Send reminder emails (clearpath) | EXTERNAL (automated) | clearpath; agent tracks cadence |
| — | PSE | **(Telephone)** Begin telephone sub-process | HUMAN (`USER PROCESS`) | — |
| 4 | PSE | **(Paper)** Print surveys, mail to client | HUMAN/EXTERNAL | — |

### RPM monitoring + training

| # | Owner | Step | Class | Automation |
|---|-------|------|-------|------------|
| 5 | RPM | Begin monitoring (monday) | AUTO/HUMAN (`USER PROCESS`) | agent posts daily response-rate summary to the item |
| 6 | RPM | Coordinate platform training per schedule (monday) | HUMAN (`USER PROCESS`) | agent drafts scheduling email |
| 7 | RPM | Email to confirm platform-training date (monday) | DRAFT | send confirmation request |
| 8 | RPM | Communicate with client + PSE as appropriate (monday) | HUMAN | notify only |
| 9 | Client | Receive training confirmation; confirm or propose new date | EXTERNAL | watch reply; reschedule Calendar event |
| 10 | RPM | Host platform training — client receives (monday) | HUMAN (`USER PROCESS`) | arm starting-soon trigger; attach materials |

### Decision: close survey?

- **Yes:** client sends approval to close → RPM receives + sends approval to
  close → PSE receives approval to close (`USER PROCESS`).
- **No:** request to keep survey open → back to RPM; clearpath fires
  additional reminder emails (EXTERNAL automated).

### Exit

- Survey closed. Hand off to **Reporting** (phase not in this document).

## What this skill does NOT do

- Send the actual invitation/reminder emails (clearpath owns this).
- Run the telephone sub-process or print/mail paper surveys (human/external).
- Host the live platform training (human).

## Notifications

- Telegram ping to RPM on: deployment started, low response-rate threshold
  crossed, training date confirmed, close-survey approval received.

## Monitoring automation idea

Step 5 ("begin monitoring") is the best candidate for genuine agent value:
a cron job that pulls response counts (via clearpath or the survey platform's
API through Composio, if available) and dispatches a daily/threshold summary,
turning a `USER PROCESS` watch task into an automated digest.
