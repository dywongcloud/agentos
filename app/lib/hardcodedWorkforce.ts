// app/lib/hardcodedWorkforce.ts
//
// A single, hardcoded, purely-VISUAL workforce used as a showcase across the
// dashboard surfaces (workflows manifest, agents canvas, workforce listing,
// office/canvas view, and the chat-logs / Logs feed). It is NOT backed by
// Redis and has NO executable workflow function — it never runs, never fires,
// and is injected only at render/read time so it always appears regardless of
// tenant state. Two Claude-Code social agents (macOS iMessage + WeChat) sit
// behind a "new chat message" trigger and share the LinkedIn / Reddit / Composio
// workbench toolkits.

import type { SubAgent, Workforce } from "@/app/lib/agents";
import type { AutomationTrigger } from "@/app/lib/automations";

export const HC_TEAM_ID = "team_hc_claudecode";
export const HC_AGENT_IMESSAGE_ID = "ag_hc_imessage";
export const HC_AGENT_WECHAT_ID = "ag_hc_wechat";

// WeChat brand mark for the trigger node ("new chat message").
export const WECHAT_LOGO = "https://cdn.simpleicons.org/wechat";

// Shared toolkit slugs — resolve to LinkedIn / Reddit logos + a Composio
// workbench chip via app/ui/toolkitLogo.ts.
const HC_TOOLKITS = ["linkedin", "reddit", "composio"];

// A stable, recent-ish creation epoch so the team sorts after older teams but
// the chat log still reads as "today".
const HC_CREATED_AT = 1717000000000; // 2024-05-29

export function hardcodedSubAgents(tenantId: string): SubAgent[] {
  return [
    {
      id: HC_AGENT_IMESSAGE_ID,
      tenantId,
      name: "macOS iMessage Agent (Claude Code)",
      emoji: "💬",
      persona:
        "Claude Code running on macOS, wired into the iMessage database. Reads new " +
        "DMs, drafts warm replies, and runs LinkedIn + Reddit outreach through the " +
        "Composio workbench — all from the desktop.",
      toolkits: HC_TOOLKITS,
      createdAt: HC_CREATED_AT,
      updatedAt: HC_CREATED_AT,
    },
    {
      id: HC_AGENT_WECHAT_ID,
      tenantId,
      name: "WeChat Claude Code",
      emoji: "🟢",
      persona:
        "Claude Code bridged into WeChat. Watches incoming chats and group threads, " +
        "replies in-language, and enriches contacts via LinkedIn + Reddit lookups " +
        "through the Composio workbench.",
      toolkits: HC_TOOLKITS,
      createdAt: HC_CREATED_AT + 1,
      updatedAt: HC_CREATED_AT + 1,
    },
  ];
}

export function hardcodedTrigger(): AutomationTrigger {
  return { kind: "chat", pattern: ".*", flags: "i" };
}

export function hardcodedWorkforce(tenantId: string): Workforce {
  return {
    id: HC_TEAM_ID,
    tenantId,
    channel: "telegram",
    sessionId: `hc:${HC_TEAM_ID}`,
    name: "iMessage + WeChat (Claude Code)",
    emoji: "🤖",
    spec:
      "Two Claude-Code chat agents — one on macOS iMessage, one on WeChat — that " +
      "react to any new chat message, draft replies, and run LinkedIn + Reddit " +
      "outreach via the Composio workbench.",
    stages: [
      {
        kind: "agents",
        agentIds: [HC_AGENT_IMESSAGE_ID, HC_AGENT_WECHAT_ID],
      },
    ],
    automationId: "auto_hc_claudecode",
    enabled: true,
    createdAt: HC_CREATED_AT,
  };
}

export type HardcodedChatLine = { who: string; text: string; group?: boolean };

// Sample outreach log surfaced as the workforce's "chat logs".
export function hardcodedChatLog(): HardcodedChatLine[] {
  return [
    { who: "James Gea", text: "Hi James, I'm Claude, Anders' assistant — wanted to reach out about…" },
    { who: "File Transfer", text: "I don't see any message content here yet — happy to take a look once…" },
    { who: "Anna", text: "Thanks Anna! Really appreciate you getting back so quickly." },
    { who: "Chris", text: "Thanks for reaching out, Chris — let's find a time this week." },
    { who: "Jimmy", text: "Hi Jimmy, thanks for reaching out. Here's what I'd suggest next…" },
    { who: "Shelley Chen (55BFF)", text: "Hi Shelley! Great to connect — loved your last post on the fund." },
    { who: "dylan wong", text: "Hi dylan, great to hear from you — let's get the demo scheduled." },
    { who: "danny herrera", text: "No worries, there may have been a mix-up — resending the deck now." },
    { who: "US Business", text: "Poshu: Ok hope we can close this before the end of the quarter.", group: true },
  ];
}
