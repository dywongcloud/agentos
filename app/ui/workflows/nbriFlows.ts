// app/ui/workflows/nbriFlows.ts
//
// Presentation-only models of NBRI's survey-delivery workflows (from
// NBRI_Workflows_1.pdf). These are NOT executable WDK workflows — they exist
// purely to render visual flow diagrams on the workflows dashboard so the
// process can be walked through with a customer.
//
// Node vocabulary mirrors the WDK primitives the customer will recognize:
//   trigger  — what starts the workflow
//   hook     — wait for an external event (Composio trigger / inbound email)
//   step     — a unit of work (tagged auto/draft/human/external)
//   sleep    — wait for time or for a human/client to act
//   parallel — steps that run concurrently
//   decision — a branch (Yes/No, mode, …)
//   loop     — re-enter an earlier point
//   end      — hand off to the next phase

export type AutoLevel = "auto" | "draft" | "human" | "external";

export type FlowNode =
  | { kind: "trigger"; label: string; detail?: string; actor?: string }
  | { kind: "hook"; label: string; detail?: string; actor?: string }
  | {
      kind: "step";
      label: string;
      detail?: string;
      actor?: string;
      auto?: AutoLevel;
    }
  | { kind: "sleep"; label: string; detail?: string }
  | {
      kind: "parallel";
      label?: string;
      lanes: {
        label: string;
        detail?: string;
        actor?: string;
        auto?: AutoLevel;
      }[];
    }
  | {
      kind: "decision";
      label: string;
      branches: { label: string; nodes: FlowNode[] }[];
    }
  | { kind: "loop"; label: string }
  | { kind: "end"; label: string };

export type Flow = {
  id: string;
  title: string;
  phase: string;
  triggerSummary: string;
  nodes: FlowNode[];
};

export const NBRI_FLOWS: Flow[] = [
  {
    id: "kickoff",
    title: "Survey Kick-Off",
    phase: "Phase 1",
    triggerSummary:
      "Deal moves to “Won” in monday Active Deals (no native Composio trigger → monday webhook or poll)",
    nodes: [
      {
        kind: "trigger",
        label: "Deal status → “Won”",
        detail: "monday Active Deals · via monday native webhook or cron poll",
        actor: "Sales",
      },
      {
        kind: "step",
        label: "Review proposal for “won” deal",
        actor: "Sales",
        auto: "human",
      },
      {
        kind: "parallel",
        label: "On Won",
        lanes: [
          {
            label: "Change stage → Won",
            detail: "monday update item status",
            actor: "Sales",
            auto: "auto",
          },
          { label: "Assign RPM", detail: "MANUAL", actor: "Sales", auto: "human" },
          {
            label: "Send retainer invoice",
            detail: "3rd-party accounting",
            actor: "Sales",
            auto: "external",
          },
        ],
      },
      { kind: "sleep", label: "Await client payment", detail: "initial invoice" },
      {
        kind: "hook",
        label: "Payment confirmation email",
        detail: "GMAIL_NEW_GMAIL_MESSAGE",
        actor: "Client",
      },
      {
        kind: "parallel",
        label: "RPM onboarding",
        lanes: [
          {
            label: "New-account admin tasks",
            detail: "monday · MANUAL",
            actor: "RPM",
            auto: "human",
          },
          {
            label: "Tasks per “won” deal notif",
            detail: "monday · MANUAL",
            actor: "RPM",
            auto: "human",
          },
        ],
      },
      {
        kind: "step",
        label: "Email request for intro call",
        actor: "RPM",
        auto: "draft",
      },
      { kind: "sleep", label: "Await meeting confirmation", detail: "client reply" },
      {
        kind: "hook",
        label: "Meeting confirmation email",
        detail: "GMAIL_NEW_GMAIL_MESSAGE",
        actor: "Client",
      },
      {
        kind: "step",
        label: "Schedule meeting",
        detail: "Google Calendar event",
        actor: "RPM",
        auto: "auto",
      },
      {
        kind: "hook",
        label: "Intro call starting soon",
        detail: "GOOGLECALENDAR_EVENT_STARTING_SOON_TRIGGER",
      },
      {
        kind: "step",
        label: "Conduct intro call",
        detail: "PowerPoint · scripted",
        actor: "RPM",
        auto: "human",
      },
      {
        kind: "decision",
        label: "OP (Organizational Psychologist) required?",
        branches: [
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Assign OP",
                detail: "monday people column",
                actor: "RPM",
                auto: "auto",
              },
              { kind: "step", label: "Email OP", actor: "RPM", auto: "draft" },
              {
                kind: "step",
                label: "Move to Deliverables",
                actor: "RPM",
                auto: "auto",
              },
            ],
          },
          {
            label: "No",
            nodes: [
              {
                kind: "step",
                label: "Straight to Deliverables",
                actor: "RPM",
                auto: "auto",
              },
            ],
          },
        ],
      },
      { kind: "end", label: "→ Pre-Design (client receives deliverables)" },
    ],
  },

  {
    id: "predesign",
    title: "Survey Pre-Design",
    phase: "Phase 2",
    triggerSummary:
      "Client returns the documentation packet by email (GMAIL_NEW_GMAIL_MESSAGE)",
    nodes: [
      {
        kind: "trigger",
        label: "Documentation packet received",
        detail: "GMAIL_NEW_GMAIL_MESSAGE · attachment",
        actor: "Client",
      },
      {
        kind: "step",
        label: "Receive documentation package",
        detail: "monday · attach files",
        actor: "RPM",
        auto: "auto",
      },
      {
        kind: "step",
        label: "Review + file packet",
        detail: "USER PROCESS TASK · agent pre-checks completeness",
        actor: "RPM",
        auto: "human",
      },
      {
        kind: "decision",
        label: "Packet complete?",
        branches: [
          {
            label: "No",
            nodes: [
              {
                kind: "step",
                label: "Request additional information",
                actor: "RPM",
                auto: "draft",
              },
              { kind: "sleep", label: "Await client info" },
              { kind: "loop", label: "Loop back to review" },
            ],
          },
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Send package → Design phase",
                detail: "monday status",
                actor: "RPM",
                auto: "auto",
              },
              {
                kind: "step",
                label: "Notify PSE “package sent”",
                actor: "RPM",
                auto: "auto",
              },
            ],
          },
        ],
      },
      {
        kind: "step",
        label: "Review ICD, call notes, previous survey",
        detail: "OP uses own processes",
        actor: "OP",
        auto: "human",
      },
      { kind: "step", label: "Create draft QDB", actor: "OP", auto: "human" },
      {
        kind: "step",
        label: "Conduct QDB meeting",
        detail: "client provides more info",
        actor: "OP",
        auto: "human",
      },
      {
        kind: "step",
        label: "Finalize draft, send to RPM",
        actor: "OP",
        auto: "draft",
      },
      {
        kind: "step",
        label: "Receive draft from OP",
        detail: "monday",
        actor: "RPM",
        auto: "auto",
      },
      {
        kind: "step",
        label: "Send draft to client",
        detail: "monday",
        actor: "RPM",
        auto: "draft",
      },
      { kind: "sleep", label: "Await client review", detail: "approve draft" },
      {
        kind: "decision",
        label: "Draft approved?",
        branches: [
          {
            label: "No",
            nodes: [
              { kind: "loop", label: "Redo OP sub-process" },
            ],
          },
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Receive approval",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
              {
                kind: "step",
                label: "Send completed draft to OP",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
            ],
          },
        ],
      },
      {
        kind: "step",
        label: "Finalize QSB, send finalized QDB",
        detail: "USER PROCESS · email",
        actor: "OP",
        auto: "human",
      },
      {
        kind: "step",
        label: "Receive QDB, file + send → Design",
        detail: "monday",
        actor: "RPM",
        auto: "auto",
      },
      { kind: "end", label: "→ Design (client receives QDB)" },
    ],
  },

  {
    id: "design",
    title: "Design",
    phase: "Phase 3",
    triggerSummary:
      "monday item status → “Design”; Gmail replies drive the 4 approval loops",
    nodes: [
      {
        kind: "trigger",
        label: "Status → “Design”",
        detail: "monday webhook or poll",
      },
      {
        kind: "step",
        label: "Start survey design + communication",
        detail: "USER PROCESS",
        actor: "PSE",
        auto: "human",
      },
      {
        kind: "step",
        label: "Send email proofs + login creds",
        actor: "PSE",
        auto: "draft",
      },
      {
        kind: "step",
        label: "Forward proofs + creds to client",
        detail: "monday",
        actor: "RPM",
        auto: "auto",
      },
      { kind: "sleep", label: "Await proof decision", detail: "client" },
      {
        kind: "hook",
        label: "Client reply",
        detail: "GMAIL_NEW_GMAIL_MESSAGE",
        actor: "Client",
      },
      {
        kind: "decision",
        label: "Proofs approved?",
        branches: [
          {
            label: "No → change loop",
            nodes: [
              {
                kind: "step",
                label: "Receive + review changes",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
              {
                kind: "step",
                label: "Make requested changes",
                detail: "USER PROCESS",
                actor: "PSE",
                auto: "human",
              },
              { kind: "loop", label: "Loop back to client" },
            ],
          },
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Record approval",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
            ],
          },
        ],
      },
      {
        kind: "decision",
        label: "Translations required?",
        branches: [
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Send text to translated.com",
                actor: "PSE",
                auto: "external",
              },
              { kind: "sleep", label: "Await English text" },
              {
                kind: "step",
                label: "Design translated surveys + emails",
                detail: "USER PROCESS",
                actor: "PSE",
                auto: "human",
              },
            ],
          },
          {
            label: "No",
            nodes: [
              {
                kind: "step",
                label: "Schedule test emails",
                actor: "PSE",
                auto: "auto",
              },
            ],
          },
        ],
      },
      {
        kind: "step",
        label: "Send new emails / proofs / surveys",
        actor: "PSE",
        auto: "draft",
      },
      {
        kind: "step",
        label: "Review test emails, send for approval",
        detail: "monday",
        actor: "RPM",
        auto: "auto",
      },
      { kind: "sleep", label: "Await test-email decision", detail: "client" },
      {
        kind: "decision",
        label: "Test emails approved?",
        branches: [
          { label: "No", nodes: [{ kind: "loop", label: "Changes → RPM → loop" }] },
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Send deployment schedule + notify",
                detail: "monday",
                actor: "RPM",
                auto: "draft",
              },
            ],
          },
        ],
      },
      { kind: "sleep", label: "Await schedule decision", detail: "client" },
      {
        kind: "decision",
        label: "Deployment schedule approved?",
        branches: [
          {
            label: "No",
            nodes: [{ kind: "loop", label: "PSE change loop" }],
          },
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Receive + send approval",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
            ],
          },
        ],
      },
      {
        kind: "step",
        label: "Schedule deployment, confirm, begin pre-deployment",
        detail: "USER PROCESS",
        actor: "PSE",
        auto: "human",
      },
      {
        kind: "step",
        label: "Email schedule confirmation, status → Deployment",
        detail: "monday",
        actor: "RPM",
        auto: "auto",
      },
      { kind: "end", label: "→ Deployment" },
    ],
  },

  {
    id: "deployment",
    title: "Deployment",
    phase: "Phase 4",
    triggerSummary:
      "monday item status → “Deployment”; Calendar + Gmail hooks for training & close",
    nodes: [
      {
        kind: "trigger",
        label: "Status → “Deployment”",
        detail: "monday webhook or poll",
      },
      {
        kind: "decision",
        label: "Deployment mode?",
        branches: [
          {
            label: "Online",
            nodes: [
              {
                kind: "step",
                label: "Schedule deployment emails",
                detail: "USER PROCESS",
                actor: "PSE",
                auto: "human",
              },
              {
                kind: "parallel",
                label: "clearpath (automated)",
                lanes: [
                  {
                    label: "Send invitation emails",
                    detail: "clearpath",
                    actor: "PSE",
                    auto: "external",
                  },
                  {
                    label: "Send reminder emails",
                    detail: "clearpath",
                    actor: "PSE",
                    auto: "external",
                  },
                ],
              },
            ],
          },
          {
            label: "Telephone",
            nodes: [
              {
                kind: "step",
                label: "Begin telephone sub-process",
                detail: "USER PROCESS",
                actor: "PSE",
                auto: "human",
              },
            ],
          },
          {
            label: "Paper",
            nodes: [
              {
                kind: "step",
                label: "Print surveys",
                detail: "USER PROCESS",
                actor: "PSE",
                auto: "human",
              },
              {
                kind: "step",
                label: "Mail to client",
                actor: "PSE",
                auto: "external",
              },
            ],
          },
        ],
      },
      {
        kind: "parallel",
        label: "RPM monitoring + training",
        lanes: [
          {
            label: "Begin monitoring",
            detail: "monday · daily digest",
            actor: "RPM",
            auto: "auto",
          },
          {
            label: "Coordinate platform training",
            detail: "monday · USER PROCESS",
            actor: "RPM",
            auto: "human",
          },
        ],
      },
      {
        kind: "step",
        label: "Email to confirm training date",
        detail: "monday",
        actor: "RPM",
        auto: "draft",
      },
      { kind: "sleep", label: "Await date confirmation", detail: "client" },
      {
        kind: "hook",
        label: "Client confirms / proposes date",
        detail: "GMAIL_NEW_GMAIL_MESSAGE",
        actor: "Client",
      },
      {
        kind: "hook",
        label: "Training starting soon",
        detail: "GOOGLECALENDAR_EVENT_STARTING_SOON_TRIGGER",
      },
      {
        kind: "step",
        label: "Host platform training",
        detail: "monday · USER PROCESS",
        actor: "RPM",
        auto: "human",
      },
      {
        kind: "decision",
        label: "Close survey?",
        branches: [
          {
            label: "No → keep open",
            nodes: [
              {
                kind: "step",
                label: "Request to keep open → RPM",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
              {
                kind: "step",
                label: "Additional reminder emails",
                detail: "clearpath",
                auto: "external",
              },
              { kind: "loop", label: "Loop monitoring" },
            ],
          },
          {
            label: "Yes",
            nodes: [
              {
                kind: "step",
                label: "Receive + send approval to close",
                detail: "monday",
                actor: "RPM",
                auto: "auto",
              },
              {
                kind: "step",
                label: "Receive approval to close",
                detail: "USER PROCESS",
                actor: "PSE",
                auto: "human",
              },
            ],
          },
        ],
      },
      { kind: "end", label: "→ Reporting" },
    ],
  },
];
