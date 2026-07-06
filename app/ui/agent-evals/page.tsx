// app/ui/agent-evals/page.tsx
//
// Eval-graph surface — the fleet-wide overall-score chart + per-agent eval
// breakdown (dimension bars, degradation flags) from the screenshots. Server
// component handles auth/tenant; EvalGraph (client) polls /api/ui/agent-evals.
// `?embed=1` strips the breadcrumb chrome so the page can be iframed as a tab
// inside the embedded workflow dashboard.

import Link from "next/link";

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";

import EvalGraph from "@/app/ui/agent-evals/EvalGraph";

export const dynamic = "force-dynamic";

type Sp = { userId?: string; embed?: string };

export default async function AgentEvalsPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/agent-evals", sp));
  const tenant = (await resolveUiTenant(sp.userId)) ?? "admin";
  const embed = sp.embed === "1" || sp.embed === "true";

  return (
    <main
      style={{
        fontFamily:
          "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
        background: embed ? "var(--card)" : "var(--muted)",
        minHeight: "100vh",
        padding: embed ? 0 : "24px 24px",
      }}
    >
      <div style={{ maxWidth: 1280, margin: "0 auto" }}>
        {!embed && (
          <>
            <div
              style={{
                display: "flex",
                gap: 16,
                fontSize: 13,
                color: "var(--muted-foreground)",
                marginBottom: 14,
              }}
            >
              <Link
                href={`/ui?tab=overview&userId=${encodeURIComponent(tenant)}`}
                style={{ color: "var(--muted-foreground)", textDecoration: "none" }}
              >
                ← Dashboard
              </Link>
              <Link
                href={`/ui/agents?userId=${encodeURIComponent(tenant)}`}
                style={{ color: "var(--muted-foreground)", textDecoration: "none" }}
              >
                Agents
              </Link>
            </div>
            <h1
              style={{
                fontSize: 26,
                fontWeight: 600,
                color: "var(--foreground)",
                margin: 0,
              }}
            >
              Agent Evals
            </h1>
            <p
              style={{
                fontSize: 13,
                color: "var(--muted-foreground)",
                marginTop: 4,
                marginBottom: 18,
              }}
            >
              Overall score trend and per-agent quality dimensions. Scores feed the governed
              self-optimization loop — run <code>/agent optimize &lt;id&gt;</code> to trigger one.
            </p>
          </>
        )}

        <EvalGraph userId={tenant} embed={embed} />
      </div>
    </main>
  );
}
