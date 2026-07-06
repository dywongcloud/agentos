// app/ui/logs/page.tsx
//
// Standalone event-stream surface. Navigated directly it wears the shared
// AppShell chrome; iframed with ?embed=1 it strips the chrome for the embedded
// workflow dashboard.

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import AppShell from "@/app/ui/shell/AppShell";
import { workspaceLabel } from "@/app/ui/shell/tabs";
import RecentActivityLive from "@/app/ui/RecentActivityLive";

export const dynamic = "force-dynamic";

type Sp = { userId?: string; embed?: string };

export default async function LogsPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/logs", sp));
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
        <RecentActivityLive userId={tenant} variant="large" limit={80} />
      </main>
    );
  }

  return (
    <AppShell
      active="logs"
      userId={tenant}
      workspaceName={workspaceLabel(tenant)}
      title="Logs"
      subtitle="Live event stream — webhooks, job ticks, and tool calls as they land."
    >
      <RecentActivityLive userId={tenant} variant="large" limit={80} />
    </AppShell>
  );
}
