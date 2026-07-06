// app/lib/accountObjective.ts
//
// The per-tenant "what is this agent-OS account FOR" objective. It frames the
// dashboard (seeds suggested KPI prompts) and the widget compiler's system
// prompt (so a vague "spend this week" resolves against the right domain).
//
// Stored as a single JSON blob per tenant — there's exactly one objective per
// account, so no index/set is needed.

import { getStore } from "@/app/lib/store";

export type ObjectiveKind =
  | "sales"
  | "marketing"
  | "seo"
  | "ppc"
  | "support"
  | "ops"
  | "custom";

export type AccountObjective = {
  tenantId: string;
  kind: ObjectiveKind;
  // Human label for the objective (defaults to a per-kind title; editable).
  label: string;
  // Optional one-line framing the user wrote ("DTC skincare brand, scaling Meta ads").
  headline?: string;
  updatedAt: number;
};

const objectiveKey = (tenantId: string) => `dash:objective:${tenantId}`;

export const OBJECTIVE_KINDS: { kind: ObjectiveKind; label: string }[] = [
  { kind: "sales", label: "Sales" },
  { kind: "marketing", label: "Marketing" },
  { kind: "seo", label: "SEO" },
  { kind: "ppc", label: "Pay-per-click" },
  { kind: "support", label: "Customer support" },
  { kind: "ops", label: "Operations" },
  { kind: "custom", label: "Custom" },
];

function defaultLabel(kind: ObjectiveKind): string {
  return OBJECTIVE_KINDS.find((k) => k.kind === kind)?.label ?? "Custom";
}

export async function getObjective(tenantId: string): Promise<AccountObjective | null> {
  const store = getStore();
  return store.get<AccountObjective>(objectiveKey(tenantId));
}

export async function setObjective(
  tenantId: string,
  input: { kind: ObjectiveKind; label?: string; headline?: string }
): Promise<AccountObjective> {
  const obj: AccountObjective = {
    tenantId,
    kind: input.kind,
    label: input.label?.trim() || defaultLabel(input.kind),
    headline: input.headline?.trim() || undefined,
    updatedAt: Date.now(),
  };
  await getStore().set(objectiveKey(tenantId), obj);
  return obj;
}

// Starter NL prompts per objective. These seed the dashboard empty-state chips
// and give the user one-tap examples that map cleanly to a data source.
const SUGGESTIONS: Record<ObjectiveKind, string[]> = {
  sales: [
    "Deals closed this week",
    "Pipeline value by stage",
    "New leads this month",
    "Win rate over time",
  ],
  marketing: [
    "Email open rate this week",
    "New subscribers this month",
    "Campaigns sent by channel",
    "Engagement rate over time",
  ],
  seo: [
    "Top landing pages by traffic",
    "Organic clicks this month",
    "Average search position",
    "Indexed pages over time",
  ],
  ppc: [
    "Cost per click this week",
    "Conversions by campaign",
    "Spend vs budget",
    "Return on ad spend over time",
  ],
  support: [
    "Open tickets by status",
    "Average first-response time",
    "Tickets resolved this week",
    "CSAT over time",
  ],
  ops: [
    "Jobs by status",
    "Automation success rate this week",
    "Average job duration",
    "Tasks completed over time",
  ],
  custom: [
    "Jobs by status",
    "Automation success rate this week",
    "Memory entries by kind",
    "Recent activity over time",
  ],
};

export function suggestedWidgetPrompts(kind: ObjectiveKind): string[] {
  return SUGGESTIONS[kind] ?? SUGGESTIONS.custom;
}
