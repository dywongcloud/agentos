// app/ui/dashboard/page.tsx
//
// The AI-composable KPI dashboard. Server component: authenticate, resolve the
// tenant, load the objective + saved widgets (with their cached data), then
// hand everything to the client <DashboardView/> which owns the command bar,
// objective selector, and the live widget grid.

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import AppShell from "@/app/ui/shell/AppShell";
import {
  getObjective,
  suggestedWidgetPrompts,
  OBJECTIVE_KINDS,
} from "@/app/lib/accountObjective";
import { listWidgets, getCachedData } from "@/app/lib/dashboards";
import type { WidgetData } from "@/app/lib/widgetSpec";
import DashboardView from "@/app/ui/dashboard/DashboardView";

export const dynamic = "force-dynamic";

type Sp = { userId?: string };

export default async function DashboardPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/dashboard", sp));

  const tenant: string = (await resolveUiTenant(sp.userId)) ?? "admin";

  const [objective, widgets] = await Promise.all([
    getObjective(tenant),
    listWidgets(tenant),
  ]);

  const dataList = await Promise.all(widgets.map((w) => getCachedData(w.id)));
  const initialData: Record<string, WidgetData | null> = Object.fromEntries(
    widgets.map((w, i) => [w.id, dataList[i]])
  );

  const kind = objective?.kind ?? "custom";
  const userLabel = tenant.includes(":") ? tenant.split(":")[1] || tenant : tenant;
  const workspaceName = userLabel === "admin" ? "Admin" : userLabel;

  return (
    <AppShell
      active="dashboard"
      userId={tenant}
      workspaceName={workspaceName}
      showHero={false}
    >
      <DashboardView
        userId={tenant}
        initialObjective={objective}
        initialSuggestions={suggestedWidgetPrompts(kind)}
        kinds={OBJECTIVE_KINDS}
        initialWidgets={widgets}
        initialData={initialData}
      />
    </AppShell>
  );
}
