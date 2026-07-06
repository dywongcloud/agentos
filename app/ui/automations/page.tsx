// app/ui/automations/page.tsx
//
// Automations ("flows") inspector. Mirrors the evals/workflows UI so a user's
// standing trigger→action rules and their run history are browsable + clickable
// the same way deep jobs are. 100% live from the auto: Redis store (no mocks).
//
//   (overview)     this tenant's automations + recent runs
//   ?id=<id>       one automation: its trigger/action + run history
//   ?runId=<id>    focused run detail: source, event, linked job, thoughts

import Link from "next/link";

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import AppShell from "@/app/ui/shell/AppShell";
import { workspaceLabel } from "@/app/ui/shell/tabs";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import {
  listByTenant,
  getAutomation,
  listRunsByRule,
  listRunsByTenant,
  getRun,
  type Automation,
  type AutomationRun,
  type AutomationTrigger,
} from "@/app/lib/automations";
import { RichText, JsonBlock, DiagnosisPanel } from "@/app/ui/RichText";

export const dynamic = "force-dynamic";

type Sp = { userId?: string; id?: string; runId?: string };

function fmtTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts).toISOString().replace("T", " ").slice(0, 19) + "Z";
}

function triggerLabel(t: AutomationTrigger): string {
  switch (t.kind) {
    case "schedule":
      return t.cron
        ? `schedule · cron ${t.cron}${t.tz ? ` (${t.tz})` : ""}`
        : `schedule · every ${Math.round((t.everyMs ?? 0) / 1000)}s`;
    case "composio":
      return `event · ${t.triggerType}`;
    case "webhook":
      return "webhook";
    case "chat":
      return `chat · /${t.pattern}/${t.flags ?? "i"}`;
  }
}

function actionLabel(a: Automation["action"]): string {
  if (a.mode === "light") return `light · ${a.steps.length} step(s)`;
  if (a.mode === "plan") return `plan · ${a.steps.length} step(s)`;
  if (a.mode === "workforce") return `workforce · ${a.workforceId}`;
  return `${a.deep ? "deep " : ""}job${a.skills?.length ? ` · skills: ${a.skills.join(", ")}` : ""}`;
}

const COLORS = {
  bg: "var(--muted)",
  card: "var(--card)",
  border: "var(--border)",
  text: "var(--foreground)",
  muted: "var(--muted-foreground)",
  green: "var(--status-completed)",
  red: "var(--destructive)",
  amber: "var(--status-cancelled)",
  blue: "var(--status-running)",
};

const styles: Record<string, React.CSSProperties> = {
  page: {
    fontFamily:
      "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
    background: COLORS.bg,
    minHeight: "100vh",
    padding: "32px 24px",
  },
  wrap: { maxWidth: 1120, margin: "0 auto" },
  topNav: { display: "flex", gap: 16, fontSize: 13, color: COLORS.muted, marginBottom: 16 },
  topNavLink: { color: COLORS.muted, textDecoration: "none" },
  h1: { fontSize: 28, fontWeight: 600, color: COLORS.text, margin: 0 },
  sub: { fontSize: 13, color: COLORS.muted, marginTop: 4, marginBottom: 20 },
  grid: { display: "grid", gridTemplateColumns: "1fr", gap: 24, marginTop: 16 },
  section: {
    background: COLORS.card,
    border: `1px solid ${COLORS.border}`,
    borderRadius: 10,
    padding: 18,
  },
  sectionTitle: {
    fontSize: 15,
    fontWeight: 600,
    color: COLORS.text,
    margin: 0,
    marginBottom: 12,
    display: "flex",
    alignItems: "center" as const,
    gap: 8,
  },
  countPill: {
    fontSize: 11,
    color: COLORS.muted,
    background: "var(--muted)",
    padding: "2px 8px",
    borderRadius: 8,
  },
  row: {
    display: "grid",
    gridTemplateColumns: "minmax(0, 1fr) auto auto",
    gap: 12,
    padding: "10px 0",
    borderBottom: `1px solid ${COLORS.border}`,
    alignItems: "center" as const,
    fontSize: 13,
    textDecoration: "none",
  },
  rowLast: { borderBottom: "none" },
  rowMain: { color: COLORS.text, minWidth: 0, wordBreak: "break-word" as const },
  rowSub: { fontSize: 11, color: COLORS.muted, marginTop: 2 },
  rowMeta: { fontSize: 12, color: COLORS.muted, whiteSpace: "nowrap" as const },
  rowTime: { fontSize: 11, color: COLORS.muted, whiteSpace: "nowrap" as const },
  empty: { fontSize: 12, color: COLORS.muted, padding: "12px 0" },
  detailBack: { fontSize: 12, color: COLORS.blue, textDecoration: "none" },
  thoughtList: {
    display: "flex",
    flexDirection: "column" as const,
    gap: 8,
    marginTop: 12,
    maxHeight: 460,
    overflowY: "auto" as const,
  },
  thoughtRow: {
    display: "grid",
    gridTemplateColumns: "90px minmax(0, 1fr)",
    gap: 10,
    fontSize: 12,
    paddingBottom: 8,
    borderBottom: `1px solid ${COLORS.border}`,
  },
  thoughtKind: {
    fontSize: 10,
    fontWeight: 600,
    textTransform: "uppercase" as const,
    color: COLORS.muted,
    letterSpacing: 0.5,
    paddingTop: 1,
  },
  thoughtText: {
    color: COLORS.text,
    wordBreak: "break-word" as const,
    whiteSpace: "pre-wrap" as const,
    lineHeight: 1.4,
  },
  pre: {
    background: "var(--muted)",
    borderRadius: 8,
    padding: 12,
    fontSize: 12,
    color: COLORS.text,
    overflowX: "auto" as const,
    whiteSpace: "pre-wrap" as const,
    wordBreak: "break-word" as const,
  },
};

function badge(color: string): React.CSSProperties {
  return {
    display: "inline-block",
    fontSize: 10,
    fontWeight: 500,
    color: "var(--card)",
    background: color,
    padding: "1px 7px",
    borderRadius: 6,
    textTransform: "uppercase",
    letterSpacing: 0.4,
  };
}

function runStatusColor(s?: string): string {
  switch (s) {
    case "ok":
      return COLORS.green;
    case "error":
      return COLORS.red;
    default:
      return COLORS.blue; // running
  }
}

function ruleStatusColor(s?: string): string {
  switch (s) {
    case "active":
      return COLORS.green;
    case "paused":
      return COLORS.amber;
    default:
      return COLORS.red; // error
  }
}

function navLink(tenant: string | null, extra: string): string {
  const base = tenant ? `userId=${encodeURIComponent(tenant)}` : "";
  if (!extra) return `/ui/automations${base ? `?${base}` : ""}`;
  return `/ui/automations?${[base, extra].filter(Boolean).join("&")}`;
}

function RunRow({
  run,
  tenant,
  last,
}: {
  run: AutomationRun;
  tenant: string | null;
  last: boolean;
}) {
  return (
    <Link
      href={navLink(tenant, `runId=${encodeURIComponent(run.id)}`)}
      style={{ ...styles.row, ...(last ? styles.rowLast : {}) }}
    >
      <div style={styles.rowMain}>
        <span style={badge(runStatusColor(run.status))}>{run.status}</span>{" "}
        <strong>{run.source}</strong>
        <div style={styles.rowSub}>
          {run.id}
          {run.jobId ? ` · job ${run.jobId}` : ""}
          {run.error ? ` · ${run.error.slice(0, 80)}` : ""}
        </div>
      </div>
      <div style={styles.rowMeta}>{run.jobId ?? ""}</div>
      <div style={styles.rowTime}>{fmtTime(run.ts)}</div>
    </Link>
  );
}

export default async function AutomationsPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/automations", sp));
  const tenant = await resolveUiTenant(sp.userId);

  // --- Run detail ----------------------------------------------------------
  if (sp.runId) {
    const run = await getRun(sp.runId);
    const rule = run ? await getAutomation(run.automationId) : null;
    return (
      <AppShell
        active="automations"
        userId={tenant ?? "admin"}
        workspaceName={workspaceLabel(tenant ?? "admin")}
        showHero={false}
      >
        <div style={styles.wrap}>
          <div style={styles.topNav}>
            <Link href={navLink(tenant, "")} style={styles.detailBack}>← All automations</Link>
            {rule ? (
              <Link href={navLink(tenant, `id=${encodeURIComponent(rule.id)}`)} style={styles.detailBack}>
                {rule.name} →
              </Link>
            ) : null}
          </div>
          <h1 style={styles.h1}>Run {sp.runId}</h1>
          {run ? (
            <>
              <p style={styles.sub}>
                <span style={badge(runStatusColor(run.status))}>{run.status}</span>{" "}
                source={run.source} · {fmtTime(run.ts)}
                {run.finishedAt ? ` · finished ${fmtTime(run.finishedAt)}` : ""}
                {run.jobId ? (
                  <>
                    {" · "}
                    <Link
                      href={`/ui/workflows?${tenant ? `userId=${encodeURIComponent(tenant)}&` : ""}jobId=${encodeURIComponent(run.jobId)}`}
                      style={{ color: COLORS.blue, textDecoration: "none" }}
                    >
                      view job →
                    </Link>
                  </>
                ) : null}
              </p>

              {run.error || run.status === "error" ? (
                <section style={{ ...styles.section, marginBottom: 16 }}>
                  <h2 style={styles.sectionTitle}>Error</h2>
                  {run.error ? (
                    <div style={{ ...styles.thoughtText, color: COLORS.red, marginBottom: 8 }}>
                      {run.error}
                    </div>
                  ) : null}
                  <DiagnosisPanel errorText={run.error || run.resultText || "error"} />
                </section>
              ) : null}

              {run.resultText ? (
                <section style={{ ...styles.section, marginBottom: 16 }}>
                  <h2 style={styles.sectionTitle}>Result</h2>
                  <RichText text={run.resultText} />
                </section>
              ) : null}

              <section style={{ ...styles.section, marginBottom: 16 }}>
                <h2 style={styles.sectionTitle}>Triggering event</h2>
                <JsonBlock value={run.event} collapse={2200} />
              </section>

              <section style={styles.section}>
                <h2 style={styles.sectionTitle}>
                  Steps <span style={styles.countPill}>{run.thoughts.length}</span>
                </h2>
                {run.thoughts.length === 0 ? (
                  <p style={styles.empty}>No steps logged.</p>
                ) : (
                  <div style={styles.thoughtList}>
                    {run.thoughts.map((t, idx) => (
                      <div key={idx} style={styles.thoughtRow}>
                        <div style={styles.thoughtKind}>{t.kind}</div>
                        <div style={{ ...styles.thoughtText, whiteSpace: "normal" }}>
                          <RichText text={t.text} collapse={1200} />
                          <div style={styles.rowTime}>{fmtTime(t.ts)}</div>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </section>
            </>
          ) : (
            <p style={styles.empty}>Run not found.</p>
          )}
        </div>
      </AppShell>
    );
  }

  // --- Automation detail ---------------------------------------------------
  if (sp.id) {
    const rule = await getAutomation(sp.id);
    const runs = rule ? await listRunsByRule(rule.id, 100) : [];
    return (
      <AppShell
        active="automations"
        userId={tenant ?? "admin"}
        workspaceName={workspaceLabel(tenant ?? "admin")}
        showHero={false}
      >
        <div style={styles.wrap}>
          <div style={styles.topNav}>
            <Link href={navLink(tenant, "")} style={styles.detailBack}>← All automations</Link>
          </div>
          {rule ? (
            <>
              <h1 style={styles.h1}>{rule.name}</h1>
              <p style={styles.sub}>
                <span style={badge(ruleStatusColor(rule.status))}>{rule.status}</span>{" "}
                {rule.enabled ? "enabled" : "paused"} · {rule.id} · fired {rule.fireCount}×
                {rule.lastFiredAt ? ` · last ${fmtTime(rule.lastFiredAt)}` : ""}
              </p>

              <section style={{ ...styles.section, marginBottom: 16 }}>
                <h2 style={styles.sectionTitle}>Rule</h2>
                <div style={styles.rowSub}>spec: {rule.spec}</div>
                <div style={{ ...styles.rowSub, marginTop: 8 }}>trigger: {triggerLabel(rule.trigger)}</div>
                <div style={{ ...styles.rowSub, marginTop: 4 }}>action: {actionLabel(rule.action)}</div>
                {rule.action.mode === "job" ? (
                  <div style={{ ...styles.pre, marginTop: 8 }}>{rule.action.instruction}</div>
                ) : null}
              </section>

              <section style={styles.section}>
                <h2 style={styles.sectionTitle}>
                  Runs <span style={styles.countPill}>{runs.length}</span>
                </h2>
                {runs.length === 0 ? (
                  <p style={styles.empty}>No runs yet. Use /automate run {rule.id} to test.</p>
                ) : (
                  <div>
                    {runs.map((r, idx) => (
                      <RunRow key={r.id} run={r} tenant={tenant} last={idx === runs.length - 1} />
                    ))}
                  </div>
                )}
              </section>
            </>
          ) : (
            <p style={styles.empty}>Automation not found.</p>
          )}
        </div>
      </AppShell>
    );
  }

  // --- Overview ------------------------------------------------------------
  const rules = tenant ? await listByTenant(tenant) : [];
  const recentRuns = tenant ? await listRunsByTenant(tenant, 40) : [];

  return (
    <AppShell
      active="automations"
      userId={tenant ?? "admin"}
      workspaceName={workspaceLabel(tenant ?? "admin")}
      showHero={false}
    >
      <div style={styles.wrap}>
        <h1 style={styles.h1}>Automations</h1>
        <p style={styles.sub}>
          Standing trigger→action rules ("flows"). Authored over chat with{" "}
          <code>/automate</code>; every firing runs as a durable, fault-tolerant
          workflow. Click an automation to see its run history, or a run to inspect
          its event, steps, and linked job.
        </p>

        <div style={styles.grid}>
          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Your automations <span style={styles.countPill}>{rules.length}</span>
            </h2>
            {rules.length === 0 ? (
              <p style={styles.empty}>
                None yet. Try: <code>/automate when I get an email from alice@acme.com, summarize it and save to my vfs</code>
              </p>
            ) : (
              <div>
                {rules.map((rule, idx) => (
                  <Link
                    key={rule.id}
                    href={navLink(tenant, `id=${encodeURIComponent(rule.id)}`)}
                    style={{ ...styles.row, ...(idx === rules.length - 1 ? styles.rowLast : {}) }}
                  >
                    <div style={styles.rowMain}>
                      <span style={badge(ruleStatusColor(rule.status))}>
                        {rule.enabled ? rule.status : "paused"}
                      </span>{" "}
                      <strong>{rule.name}</strong>
                      <div style={styles.rowSub}>
                        {triggerLabel(rule.trigger)} → {actionLabel(rule.action)}
                      </div>
                    </div>
                    <div style={styles.rowMeta}>fired {rule.fireCount}×</div>
                    <div style={styles.rowTime}>{fmtTime(rule.lastFiredAt)}</div>
                  </Link>
                ))}
              </div>
            )}
          </section>

          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Recent runs <span style={styles.countPill}>{recentRuns.length}</span>
            </h2>
            {recentRuns.length === 0 ? (
              <p style={styles.empty}>No runs yet.</p>
            ) : (
              <div>
                {recentRuns.map((r, idx) => (
                  <RunRow key={r.id} run={r} tenant={tenant} last={idx === recentRuns.length - 1} />
                ))}
              </div>
            )}
          </section>
        </div>
      </div>
    </AppShell>
  );
}
