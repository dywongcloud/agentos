// app/ui/activity/page.tsx
//
// Standalone audit-log surface. Navigated directly it wears the shared AppShell
// chrome; iframed with ?embed=1 it strips the chrome for the embedded workflow
// dashboard.

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import AppShell from "@/app/ui/shell/AppShell";
import { workspaceLabel } from "@/app/ui/shell/tabs";
import AuditLog from "@/app/ui/AuditLog";

export const dynamic = "force-dynamic";

type Sp = { userId?: string; embed?: string };

export default async function ActivityPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/activity", sp));
  const tenant = (await resolveUiTenant(sp.userId)) ?? "admin";
  const embed = sp.embed === "1" || sp.embed === "true";

  if (embed) {
    return (
      <main
        style={{
          fontFamily:
            "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
          background: "var(--card)",
          minHeight: "100vh",
          padding: 18,
        }}
      >
        <AuditLog userId={tenant} limit={120} />
      </main>
    );
  }

  return (
    <AppShell
      active="activity"
      userId={tenant}
      workspaceName={workspaceLabel(tenant)}
      title="Activity"
      subtitle="Audit log of tenant actions, webhooks, and job state changes."
    >
      <AuditLog userId={tenant} limit={120} />
    </AppShell>
  );
}
