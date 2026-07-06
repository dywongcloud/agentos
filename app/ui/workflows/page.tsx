// app/ui/workflows/page.tsx
//
// Real workflow inspector: shows running + recent jobs, code projects, and
// trigger webhook deliveries for the selected tenant. Sources are 100% live
// (jobStore, codeProjectStore, composio:webhook:log). No mocks anywhere.
//
// If `?jobId=` or `?projectId=` is passed, shows a focused detail panel for
// that workflow with the thought stream / task history. Otherwise renders
// the overview list.

import Link from "next/link";

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import DylanClawLogo from "@/app/ui/DylanClawLogo";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import {
  listActiveJobs,
  listRecentJobs,
  getJobMeta,
  getThoughts,
} from "@/app/lib/jobStore";
import {
  listActiveCodeProjects,
  listRecentCodeProjects,
  getCodeProject,
  getCodeTasks,
  getCodeThoughts,
} from "@/app/lib/codeProjectStore";
import { getRecentWebhookHits } from "@/app/lib/composioWebhook";
import {
  listWorkforcesByTenant,
  getAgentsByIds,
} from "@/app/lib/agents";
import { getAutomation } from "@/app/lib/automations";
import { RichText, DiagnosisPanel } from "@/app/ui/RichText";
import { NBRI_FLOWS } from "./nbriFlows";
import { FlowDiagram, FlowLegend } from "./FlowDiagram";
import { workforceToFlow } from "./workforceFlows";

export const dynamic = "force-dynamic";

type Sp = {
  userId?: string;
  jobId?: string;
  projectId?: string;
  view?: string;
};

function fmtTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts).toISOString().replace("T", " ").slice(0, 19) + "Z";
}

function dollars(n?: number | null): string {
  if (typeof n !== "number") return "$0.000";
  return "$" + n.toFixed(3);
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
  topNav: {
    display: "flex",
    gap: 16,
    fontSize: 13,
    color: COLORS.muted,
    marginBottom: 16,
  },
  topNavLink: { color: COLORS.muted, textDecoration: "none" },
  h1: { fontSize: 28, fontWeight: 600, color: COLORS.text, margin: 0 },
  sub: { fontSize: 13, color: COLORS.muted, marginTop: 4, marginBottom: 20 },
  grid: {
    display: "grid",
    gridTemplateColumns: "1fr",
    gap: 24,
    marginTop: 16,
  },
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
  },
  rowLast: { borderBottom: "none" },
  rowMain: { color: COLORS.text, minWidth: 0, wordBreak: "break-word" as const },
  rowSub: { fontSize: 11, color: COLORS.muted, marginTop: 2 },
  rowMeta: { fontSize: 12, color: COLORS.muted, whiteSpace: "nowrap" as const },
  rowTime: { fontSize: 11, color: COLORS.muted, whiteSpace: "nowrap" as const },
  empty: {
    fontSize: 12,
    color: COLORS.muted,
    padding: "12px 0",
  },
  detailBack: {
    fontSize: 12,
    color: COLORS.blue,
    textDecoration: "none",
  },
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
    gridTemplateColumns: "70px minmax(0, 1fr)",
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

function statusColor(s?: string): string {
  switch (s) {
    case "done":
      return COLORS.green;
    case "failed":
      return COLORS.red;
    case "pending":
    case "clarifying":
    case "planning":
      return COLORS.amber;
    default:
      return COLORS.blue;
  }
}

export default async function WorkflowsPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/workflows", sp));
  const tenant = await resolveUiTenant(sp.userId);

  // --- Skill diagrams (presentation) ---------------------------------------
  // Visual process maps from the NBRI workflows PDF. Tenant-independent — works
  // even before any inbound message, so it's safe to show for a customer demo.
  if (sp.view === "diagrams") {
    const liveHref = `/ui/workflows${tenant ? `?userId=${encodeURIComponent(tenant)}` : ""}`;
    return (
      <main style={styles.page}>
        <div style={styles.wrap}>
          <Link href="/home" style={{ display: "inline-flex", alignItems: "center", color: "var(--foreground)", textDecoration: "none", marginBottom: 14 }} aria-label="DylanClaw"><DylanClawLogo height={22} /></Link>
          <div style={styles.topNav}>
            <Link href={liveHref} style={styles.topNavLink}>← Live workflows</Link>
          </div>
          <h1 style={styles.h1}>Skill workflows — NBRI</h1>
          <p style={styles.sub}>
            Visual process maps from <code>NBRI_Workflows_1.pdf</code>. Each flow
            shows triggers, steps, hooks (event waits), parallel work, sleeps,
            and decision branches.
          </p>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 10, marginBottom: 14 }}>
            {NBRI_FLOWS.map((f) => (
              <a
                key={f.id}
                href={`#flow-${f.id}`}
                style={{
                  fontSize: 12,
                  color: "var(--status-running)",
                  textDecoration: "none",
                  border: `1px solid ${COLORS.border}`,
                  borderRadius: 8,
                  padding: "4px 10px",
                }}
              >
                {f.phase}: {f.title}
              </a>
            ))}
          </div>
          <FlowLegend />
          <div style={{ display: "flex", flexDirection: "column", gap: 24, marginTop: 8 }}>
            {NBRI_FLOWS.map((f) => (
              <FlowDiagram key={f.id} flow={f} />
            ))}
          </div>
        </div>
      </main>
    );
  }

  if (!tenant) {
    return (
      <main style={styles.page}>
        <div style={styles.wrap}>
          <h1 style={styles.h1}>Workflows</h1>
          <p style={styles.sub}>
            No tenant yet. Pair Telegram (or send any inbound message) to populate.
          </p>
        </div>
      </main>
    );
  }

  // --- Workforce diagrams (runtime) ----------------------------------------
  // Live process maps for the tenant's sub-agent teams. Built at request time
  // from the stored Workforce records + each agent's persona/toolkits + the
  // trigger rule — so a team created seconds ago renders here with no rebuild.
  if (sp.view === "workforces") {
    const liveHref = `/ui/workflows?userId=${encodeURIComponent(tenant)}`;
    const teams = await listWorkforcesByTenant(tenant);
    const flows = await Promise.all(
      teams.map(async (team) => {
        const memberIds = Array.from(
          new Set(
            team.stages.flatMap((s) =>
              s.kind === "route" ? s.candidateAgentIds : s.agentIds
            )
          )
        );
        const [agents, rule] = await Promise.all([
          getAgentsByIds(memberIds),
          team.automationId ? getAutomation(team.automationId) : Promise.resolve(null),
        ]);
        return workforceToFlow(team, agents, rule);
      })
    );
    return (
      <main style={styles.page}>
        <div style={styles.wrap}>
          <Link href="/home" style={{ display: "inline-flex", alignItems: "center", color: "var(--foreground)", textDecoration: "none", marginBottom: 14 }} aria-label="DylanClaw"><DylanClawLogo height={22} /></Link>
          <div style={styles.topNav}>
            <Link href={liveHref} style={styles.topNavLink}>← Live workflows</Link>
            <Link href={`/ui/agents?userId=${encodeURIComponent(tenant)}`} style={{ ...styles.topNavLink, color: "var(--status-running)" }}>
              Workforce canvas →
            </Link>
          </div>
          <h1 style={styles.h1}>Workforce diagrams</h1>
          <p style={styles.sub}>
            Generated flow diagrams for each workforce belonging to{" "}
            <code>{tenant}</code>. Each shows the trigger, the ordered stages,
            parallel agents, AI-routing branches, and every agent&apos;s unique
            task (from its persona). Built live — new teams appear with no rebuild.
          </p>
          {flows.length === 0 ? (
            <p style={styles.empty}>
              No workforces yet. Create one on the{" "}
              <Link href={`/ui/agents?userId=${encodeURIComponent(tenant)}`} style={{ color: "var(--status-running)" }}>
                workforce canvas
              </Link>
              .
            </p>
          ) : (
            <>
              <div style={{ display: "flex", flexWrap: "wrap", gap: 10, marginBottom: 14 }}>
                {flows.map((f) => (
                  <a
                    key={f.id}
                    href={`#flow-${f.id}`}
                    style={{
                      fontSize: 12,
                      color: "var(--status-running)",
                      textDecoration: "none",
                      border: `1px solid ${COLORS.border}`,
                      borderRadius: 8,
                      padding: "4px 10px",
                    }}
                  >
                    {f.title}
                  </a>
                ))}
              </div>
              <FlowLegend />
              <div style={{ display: "flex", flexDirection: "column", gap: 24, marginTop: 8 }}>
                {flows.map((f) => (
                  <FlowDiagram key={f.id} flow={f} />
                ))}
              </div>
            </>
          )}
        </div>
      </main>
    );
  }

  // --- Detail views --------------------------------------------------------

  if (sp.jobId) {
    const meta = await getJobMeta(sp.jobId);
    const thoughts = await getThoughts(sp.jobId, { limit: 200 });
    return (
      <main style={styles.page}>
        <div style={styles.wrap}>
          <Link href="/home" style={{ display: "inline-flex", alignItems: "center", color: "var(--foreground)", textDecoration: "none", marginBottom: 14 }} aria-label="DylanClaw"><DylanClawLogo height={22} /></Link>
          <div style={styles.topNav}>
            <Link href={`/ui/workflows?userId=${encodeURIComponent(tenant)}`} style={styles.detailBack}>
              ← All workflows
            </Link>
</div>
          <h1 style={styles.h1}>Job {sp.jobId}</h1>
          {meta ? (
            <>
              <p style={styles.sub}>
                <span style={badge(statusColor(meta.status))}>{meta.status}</span>{" "}
                kind={meta.kind} · cost {dollars(meta.estimatedCost)} ·{" "}
                {meta.escalated ? "escalated · " : ""}
                {meta.depthPasses ?? 0} depth pass(es) · created {fmtTime(meta.createdAt)}
                {meta.finishedAt ? ` · finished ${fmtTime(meta.finishedAt)}` : ""}
              </p>
              <section style={styles.section}>
                <h2 style={styles.sectionTitle}>
                  Prompt
                </h2>
                <RichText text={meta.prompt} />
              </section>
              {meta.status === "failed" || meta.error ? (
                <section style={{ ...styles.section, marginTop: 16 }}>
                  <h2 style={styles.sectionTitle}>Failure</h2>
                  {meta.error ? (
                    <div style={{ ...styles.thoughtText, color: COLORS.red, marginBottom: 8 }}>
                      {meta.error}
                    </div>
                  ) : null}
                  <DiagnosisPanel errorText={meta.error || meta.resultText || meta.status} />
                </section>
              ) : null}
              {meta.resultText ? (
                <section style={{ ...styles.section, marginTop: 16 }}>
                  <h2 style={styles.sectionTitle}>Result ({meta.resultText.length} chars)</h2>
                  <RichText text={meta.resultText} />
                </section>
              ) : null}
              <section style={{ ...styles.section, marginTop: 16 }}>
                <h2 style={styles.sectionTitle}>
                  Thought stream <span style={styles.countPill}>{thoughts.length}</span>
                </h2>
                <div style={styles.thoughtList}>
                  {thoughts.slice().reverse().map((t, idx) => (
                    <div key={idx} style={styles.thoughtRow}>
                      <div style={styles.thoughtKind}>{t.kind}</div>
                      <div style={{ ...styles.thoughtText, whiteSpace: "normal" }}>
                        <RichText text={t.text} collapse={1200} />
                      </div>
                    </div>
                  ))}
                </div>
              </section>
            </>
          ) : (
            <p style={styles.empty}>Job not found.</p>
          )}
        </div>
      </main>
    );
  }

  if (sp.projectId) {
    const proj = await getCodeProject(sp.projectId);
    const tasks = await getCodeTasks(sp.projectId, 30);
    const log = await getCodeThoughts(sp.projectId, { limit: 100 });
    return (
      <main style={styles.page}>
        <div style={styles.wrap}>
          <Link href="/home" style={{ display: "inline-flex", alignItems: "center", color: "var(--foreground)", textDecoration: "none", marginBottom: 14 }} aria-label="DylanClaw"><DylanClawLogo height={22} /></Link>
          <div style={styles.topNav}>
            <Link href={`/ui/workflows?userId=${encodeURIComponent(tenant)}`} style={styles.detailBack}>
              ← All workflows
            </Link>
</div>
          <h1 style={styles.h1}>Code project {sp.projectId}</h1>
          {proj ? (
            <>
              <p style={styles.sub}>
                <span style={badge(statusColor(proj.status))}>{proj.status}</span>{" "}
                engine={proj.engine} · turn {proj.turnCount} · created {fmtTime(proj.createdAt)}
                {proj.repoUrl ? ` · repo ${proj.repoUrl}` : ""}
                {proj.pushedBranch ? ` · pushed ${proj.pushedBranch}` : ""}
              </p>
              <section style={styles.section}>
                <h2 style={styles.sectionTitle}>Title</h2>
                <div style={styles.thoughtText}>{proj.title}</div>
              </section>
              {proj.lastOutput ? (
                <section style={{ ...styles.section, marginTop: 16 }}>
                  <h2 style={styles.sectionTitle}>Last output</h2>
                  <RichText text={proj.lastOutput} />
                </section>
              ) : null}
              {(() => {
                const lastErr = tasks.find((t) => t.error)?.error;
                return lastErr ? (
                  <section style={{ ...styles.section, marginTop: 16 }}>
                    <h2 style={styles.sectionTitle}>Failure</h2>
                    <div style={{ ...styles.thoughtText, color: COLORS.red, marginBottom: 8 }}>
                      {lastErr}
                    </div>
                    <DiagnosisPanel errorText={lastErr} />
                  </section>
                ) : null;
              })()}
              <section style={{ ...styles.section, marginTop: 16 }}>
                <h2 style={styles.sectionTitle}>
                  Tasks <span style={styles.countPill}>{tasks.length}</span>
                </h2>
                <div style={styles.thoughtList}>
                  {tasks.map((t, idx) => (
                    <div key={idx} style={styles.thoughtRow}>
                      <div style={styles.thoughtKind}>
                        #{t.turn} {t.status}
                      </div>
                      <div style={styles.thoughtText}>
                        {t.task}
                        {t.outputPreview ? `\n\n→ ${t.outputPreview}` : ""}
                        {t.error ? `\n\nerror: ${t.error}` : ""}
                      </div>
                    </div>
                  ))}
                </div>
              </section>
              <section style={{ ...styles.section, marginTop: 16 }}>
                <h2 style={styles.sectionTitle}>
                  Log <span style={styles.countPill}>{log.length}</span>
                </h2>
                <div style={styles.thoughtList}>
                  {log.slice().reverse().map((l, idx) => (
                    <div key={idx} style={styles.thoughtRow}>
                      <div style={styles.thoughtKind}>{l.kind}</div>
                      <div style={styles.thoughtText}>{l.text}</div>
                    </div>
                  ))}
                </div>
              </section>
            </>
          ) : (
            <p style={styles.empty}>Project not found.</p>
          )}
        </div>
      </main>
    );
  }

  // --- Overview ------------------------------------------------------------

  const [activeJobIds, recentJobIds, activeProjIds, recentProjIds, hits] =
    await Promise.all([
      listActiveJobs(tenant),
      listRecentJobs(tenant, 15),
      listActiveCodeProjects(tenant),
      listRecentCodeProjects(tenant, 15),
      getRecentWebhookHits(25),
    ]);

  // Hydrate jobs
  const jobSet = new Set<string>();
  const allJobIds = [...activeJobIds, ...recentJobIds].filter(
    (j) => !jobSet.has(j) && (jobSet.add(j), true)
  );
  const jobMetas = await Promise.all(allJobIds.map((id) => getJobMeta(id)));
  const jobs = jobMetas
    .filter((m): m is NonNullable<typeof m> => !!m)
    .sort((a, b) => (b.updatedAt ?? b.createdAt) - (a.updatedAt ?? a.createdAt));

  // Hydrate code projects
  const projSet = new Set<string>();
  const allProjIds = [...activeProjIds, ...recentProjIds].filter(
    (p) => !projSet.has(p) && (projSet.add(p), true)
  );
  const projMetas = await Promise.all(allProjIds.map((id) => getCodeProject(id)));
  const projs = projMetas
    .filter((p): p is NonNullable<typeof p> => !!p)
    .sort((a, b) => (b.updatedAt ?? b.createdAt) - (a.updatedAt ?? a.createdAt));

  // Tenant-scoped webhook hits (others may exist if multiple tenants share
  // the same Composio project — keep only ones that match or have no tenant).
  const tenantHits = hits.filter((h) => !h.tenantId || h.tenantId === tenant);
  return (
    <main style={styles.page}>
      <div style={styles.wrap}>
        <Link href="/home" style={{ display: "inline-flex", alignItems: "center", color: "var(--foreground)", textDecoration: "none", marginBottom: 14 }} aria-label="DylanClaw"><DylanClawLogo height={22} /></Link>
        <div style={styles.topNav}>
          <Link href={`/ui?userId=${encodeURIComponent(tenant)}`} style={styles.topNavLink}>
            ← Dashboard
          </Link>
          <Link href={`/ui/workflows?userId=${encodeURIComponent(tenant)}&view=workforces`} style={{ ...styles.topNavLink, color: "var(--status-running)" }}>
            Workforce diagrams →
          </Link>
          <Link href={`/ui/workflows?userId=${encodeURIComponent(tenant)}&view=diagrams`} style={{ ...styles.topNavLink, color: "var(--status-running)" }}>
            Skill diagrams (NBRI) →
          </Link>
</div>
        <h1 style={styles.h1}>Workflows</h1>
        <p style={styles.sub}>
          Live view of jobs, code projects, and trigger deliveries for{" "}
          <code>{tenant}</code>. Updates every page load.
        </p>

        <div style={styles.grid}>
          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Jobs <span style={styles.countPill}>{jobs.length}</span>
            </h2>
            {jobs.length === 0 ? (
              <p style={styles.empty}>No jobs yet for this tenant.</p>
            ) : (
              <div>
                {jobs.map((j, idx) => {
                  const last = idx === jobs.length - 1;
                  return (
                    <Link
                      key={j.jobId}
                      href={`/ui/workflows?userId=${encodeURIComponent(tenant)}&jobId=${encodeURIComponent(j.jobId)}`}
                      style={{ ...styles.row, ...(last ? styles.rowLast : {}), textDecoration: "none" }}
                    >
                      <div style={styles.rowMain}>
                        <span style={badge(statusColor(j.status))}>{j.status}</span>{" "}
                        <strong>{j.jobId}</strong> · {j.prompt.slice(0, 110)}
                        <div style={styles.rowSub}>
                          {j.kind}
                          {j.escalated ? " · escalated" : ""}
                          {typeof j.depthPasses === "number" && j.depthPasses > 0
                            ? ` · ${j.depthPasses} depth pass(es)`
                            : ""}
                        </div>
                      </div>
                      <div style={styles.rowMeta}>{dollars(j.estimatedCost)}</div>
                      <div style={styles.rowTime}>{fmtTime(j.updatedAt ?? j.createdAt)}</div>
                    </Link>
                  );
                })}
              </div>
            )}
          </section>

          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Code projects <span style={styles.countPill}>{projs.length}</span>
            </h2>
            {projs.length === 0 ? (
              <p style={styles.empty}>No code projects yet.</p>
            ) : (
              <div>
                {projs.map((p, idx) => {
                  const last = idx === projs.length - 1;
                  return (
                    <Link
                      key={p.projectId}
                      href={`/ui/workflows?userId=${encodeURIComponent(tenant)}&projectId=${encodeURIComponent(p.projectId)}`}
                      style={{ ...styles.row, ...(last ? styles.rowLast : {}), textDecoration: "none" }}
                    >
                      <div style={styles.rowMain}>
                        <span style={badge(statusColor(p.status))}>{p.status}</span>{" "}
                        <strong>{p.projectId}</strong> · {p.title.slice(0, 110)}
                        <div style={styles.rowSub}>
                          {p.engine} · turn {p.turnCount}
                          {p.repoUrl ? ` · ${p.repoUrl}` : ""}
                        </div>
                      </div>
                      <div style={styles.rowMeta}>{p.engine}</div>
                      <div style={styles.rowTime}>{fmtTime(p.updatedAt ?? p.createdAt)}</div>
                    </Link>
                  );
                })}
              </div>
            )}
          </section>

          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Trigger deliveries <span style={styles.countPill}>{tenantHits.length}</span>
            </h2>
            {tenantHits.length === 0 ? (
              <p style={styles.empty}>No trigger events received yet.</p>
            ) : (
              <div>
                {tenantHits.map((h, idx) => {
                  const last = idx === tenantHits.length - 1;
                  return (
                    <div key={`${h.ts}-${idx}`} style={{ ...styles.row, ...(last ? styles.rowLast : {}) }}>
                      <div style={styles.rowMain}>
                        <span style={badge(h.ok ? COLORS.green : COLORS.red)}>
                          {h.ok ? "delivered" : "dropped"}
                        </span>{" "}
                        {h.slug ?? "(unknown trigger)"}
                        {!h.ok && h.error ? (
                          <div style={styles.rowSub}>{h.error}</div>
                        ) : h.triggerId ? (
                          <div style={styles.rowSub}>trigger {h.triggerId}</div>
                        ) : null}
                      </div>
                      <div style={styles.rowMeta}></div>
                      <div style={styles.rowTime}>{fmtTime(h.ts)}</div>
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </div>
      </div>
    </main>
  );
}
