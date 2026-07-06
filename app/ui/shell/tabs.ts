// app/ui/shell/tabs.ts
//
// Single source of truth for the top-level navigation. Every /ui surface uses
// this so the tab bar, labels, and link destinations stay consistent across
// the dashboard, the legacy tab bodies in page.tsx, and the dedicated routes
// (/ui/evals, /ui/automations, /ui/agents).

export type TabKey =
  | "dashboard"
  | "overview"
  | "files"
  | "integrations"
  | "workflows"
  | "evals"
  | "automations"
  | "agents"
  | "logs"
  | "activity"
  | "domains"
  | "usage"
  | "settings";

// Order shown in the tab bar. Dashboard leads.
export const TOP_TABS: TabKey[] = [
  "dashboard",
  "overview",
  "files",
  "integrations",
  "workflows",
  "evals",
  "automations",
  "agents",
  "logs",
  "activity",
  "domains",
  "usage",
  "settings",
];

// Where each tab lives. Tabs whose body renders inside page.tsx point at
// /ui?tab=X; tabs with a dedicated route point straight at it (no redirect
// hop); workflows serves the upstream WDK dashboard mounted at root.
const TAB_BASE_PATH: Record<TabKey, string> = {
  dashboard: "/ui/dashboard",
  overview: "/ui",
  files: "/ui",
  integrations: "/ui",
  workflows: "/",
  evals: "/ui/evals",
  automations: "/ui/automations",
  agents: "/ui/agents",
  logs: "/ui",
  activity: "/ui",
  domains: "/ui",
  usage: "/ui",
  settings: "/ui",
};

export function tabLabel(tab: TabKey): string {
  return tab[0]!.toUpperCase() + tab.slice(1);
}

// Human workspace name from a tenant id ("telegram:dylan" → "dylan", "admin"
// → "Admin"). Shared by every AppShell-wrapped page so the topbar/avatar match.
export function workspaceLabel(tenant: string): string {
  const label = tenant.includes(":") ? tenant.split(":")[1] || tenant : tenant;
  return label === "admin" ? "Admin" : label;
}

export function navHref(tab: TabKey, userId: string, q?: string): string {
  const base = TAB_BASE_PATH[tab];
  const params = new URLSearchParams();

  if (base === "/ui") {
    params.set("tab", tab);
    if (userId) params.set("userId", userId);
    if (q) params.set("q", q);
  } else if (base === "/") {
    // Upstream WDK dashboard at root — it owns its own routing, no params.
    return "/";
  } else {
    // Dedicated route — carry the tenant so it resolves the same workspace.
    if (userId) params.set("userId", userId);
  }

  const qs = params.toString();
  return qs ? `${base}?${qs}` : base;
}
