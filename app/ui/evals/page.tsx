// app/ui/evals/page.tsx
//
// Eval inspector: mirrors the workflows/jobs UI so eval suites + runs are
// browsable and clickable the same way deep jobs are. Sources are 100% live
// from the nx_evals: Redis store (no mocks).
//
//   (overview)        suite pass-rate cards + recent runs across all suites
//   ?suite=<name>     recent runs for one suite
//   ?runId=<id>       focused detail: input, actual output, graders, job link

import Link from "next/link";

import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import AppShell from "@/app/ui/shell/AppShell";
import { workspaceLabel } from "@/app/ui/shell/tabs";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import {
  listSuites,
  suiteSummary,
  listRuns,
  getRun,
  type SuiteSummary,
} from "@/app/lib/evals/store";
import type { EvalRun } from "@/app/lib/evals/types";
import { RichText, ToolCallLine, DiagnosisPanel } from "@/app/ui/RichText";

export const dynamic = "force-dynamic";

type Sp = {
  userId?: string;
  suite?: string;
  runId?: string;
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
  suiteGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))",
    gap: 12,
  },
  suiteCard: {
    border: `1px solid ${COLORS.border}`,
    borderRadius: 10,
    padding: 14,
    textDecoration: "none",
    display: "block",
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

function evalStatusColor(s?: string): string {
  switch (s) {
    case "pass":
      return COLORS.green;
    case "fail":
      return COLORS.red;
    case "partial":
      return COLORS.amber;
    default:
      return COLORS.blue; // error
  }
}

function navLink(tenant: string | null, extra: string): string {
  const base = tenant ? `userId=${encodeURIComponent(tenant)}` : "";
  if (!extra) return `/ui/evals${base ? `?${base}` : ""}`;
  return `/ui/evals?${[base, extra].filter(Boolean).join("&")}`;
}

function pct(n: number): string {
  return `${Math.round(n * 100)}%`;
}

function RunRow({
  run,
  tenant,
  last,
}: {
  run: EvalRun;
  tenant: string | null;
  last: boolean;
}) {
  const passed = run.grades.filter((g) => g.pass).length;
  return (
    <Link
      href={navLink(tenant, `runId=${encodeURIComponent(run.id)}`)}
      style={{ ...styles.row, ...(last ? styles.rowLast : {}), textDecoration: "none" }}
    >
      <div style={styles.rowMain}>
        <span style={badge(evalStatusColor(run.status))}>{run.status}</span>{" "}
        <strong>{run.suite}</strong> · {run.input.goal.slice(0, 100)}
        <div style={styles.rowSub}>
          {run.id} · {passed}/{run.grades.length} graders
          {run.input.modality ? ` · ${run.input.modality}` : ""}
          {run.actual.costUsd ? ` · ${dollars(run.actual.costUsd)}` : ""}
        </div>
      </div>
      <div style={styles.rowMeta}>{run.jobId ?? ""}</div>
      <div style={styles.rowTime}>{fmtTime(run.ts)}</div>
    </Link>
  );
}

export default async function EvalsPage({
  searchParams,
}: {
  searchParams?: Promise<Sp>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui/evals", sp));
  const tenant = await resolveUiTenant(sp.userId);

  // --- Run detail ----------------------------------------------------------
  if (sp.runId) {
    const run = await getRun(sp.runId);
    return (
      <AppShell
        active="evals"
        userId={tenant ?? "admin"}
        workspaceName={workspaceLabel(tenant ?? "admin")}
        showHero={false}
      >
        <div style={styles.wrap}>
          <div style={styles.topNav}>
            <Link href={navLink(tenant, "")} style={styles.detailBack}>
              ← All evals
            </Link>
          </div>
          <h1 style={styles.h1}>Eval run {sp.runId}</h1>
          {run ? (
            <>
              <p style={styles.sub}>
                <span style={badge(evalStatusColor(run.status))}>{run.status}</span>{" "}
                suite=<Link href={navLink(tenant, `suite=${encodeURIComponent(run.suite)}`)} style={{ color: COLORS.blue, textDecoration: "none" }}>{run.suite}</Link>{" "}
                · case {run.caseId} · {fmtTime(run.ts)}
                {run.deployId ? ` · deploy ${run.deployId.slice(0, 8)}` : ""}
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

              {run.input.meta?.flow ? (
                <section style={{ ...styles.section, marginBottom: 16 }}>
                  <h2 style={styles.sectionTitle}>OpenAI eval — {String(run.input.meta.flowName ?? run.input.meta.flow)}</h2>
                  {run.input.meta.blurb ? (
                    <div style={{ ...styles.rowSub, marginBottom: 8 }}>{String(run.input.meta.blurb)}</div>
                  ) : null}
                  <div style={styles.rowSub}>
                    flow={String(run.input.meta.flow)}
                    {run.input.meta.trigger ? ` · trigger: ${String(run.input.meta.trigger)}` : ""}
                  </div>
                  <div style={{ ...styles.rowSub, marginTop: 6 }}>
                    {run.input.meta.evalModel ? `eval model=${String(run.input.meta.evalModel)}` : ""}
                    {run.input.meta.graderModel ? ` · grader=${String(run.input.meta.graderModel)}` : ""}
                    {typeof run.input.meta.itemCount === "number" ? ` · ${run.input.meta.itemCount} item(s)` : ""}
                    {typeof run.input.meta.avgScore === "number" ? ` · avg ${Number(run.input.meta.avgScore).toFixed(2)}/7` : ""}
                  </div>
                  {run.input.meta.providerSubstituted ? (
                    <div style={{ ...styles.rowSub, marginTop: 6, color: COLORS.amber }}>
                      note: prod runs on {String(run.input.meta.realProvider)} — sampled on an OpenAI stand-in for this eval.
                    </div>
                  ) : null}
                  {run.input.meta.pending ? (
                    <div style={{ ...styles.rowSub, marginTop: 6, color: COLORS.blue }}>
                      run in progress — refresh to see grades.
                    </div>
                  ) : null}
                  {run.input.meta.evalUrl ? (
                    <div style={{ marginTop: 8 }}>
                      <a
                        href={String(run.input.meta.evalUrl)}
                        target="_blank"
                        rel="noreferrer"
                        style={{ color: COLORS.blue, textDecoration: "none" }}
                      >
                        open on platform.openai.com →
                      </a>
                      {run.input.meta.openaiRunId ? (
                        <span style={{ ...styles.rowSub, marginLeft: 10 }}>
                          run {String(run.input.meta.openaiRunId)}
                        </span>
                      ) : null}
                    </div>
                  ) : null}
                </section>
              ) : null}

              <section style={styles.section}>
                <h2 style={styles.sectionTitle}>Goal</h2>
                <RichText text={run.input.goal} />
                {(run.input.modality || run.input.channel) && (
                  <div style={{ ...styles.rowSub, marginTop: 8 }}>
                    {run.input.modality ? `modality=${run.input.modality}` : ""}
                    {run.input.channel ? ` · channel=${run.input.channel}` : ""}
                  </div>
                )}
              </section>

              <section style={{ ...styles.section, marginTop: 16 }}>
                <h2 style={styles.sectionTitle}>
                  Graders <span style={styles.countPill}>{run.grades.length}</span>
                </h2>
                <div style={styles.thoughtList}>
                  {run.grades.map((g, idx) => (
                    <div key={idx} style={styles.thoughtRow}>
                      <div style={styles.thoughtKind}>
                        <span style={badge(g.pass ? COLORS.green : COLORS.red)}>
                          {g.pass ? "pass" : "fail"}
                        </span>
                      </div>
                      <div style={{ ...styles.thoughtText, whiteSpace: "normal" }}>
                        <strong>{g.name}</strong> ({g.grader}
                        {typeof g.score === "number" ? ` · ${g.score.toFixed(2)}` : ""})
                        {g.notes ? <RichText text={g.notes} collapse={900} /> : null}
                      </div>
                    </div>
                  ))}
                  {run.grades.length === 0 ? (
                    <p style={styles.empty}>No graders recorded.</p>
                  ) : null}
                </div>
              </section>

              <section style={{ ...styles.section, marginTop: 16 }}>
                <h2 style={styles.sectionTitle}>Actual output</h2>
                <div style={{ ...styles.rowSub, marginBottom: 8 }}>
                  jobStatus={run.actual.jobStatus ?? "—"}
                  {typeof run.actual.iterations === "number" ? ` · ${run.actual.iterations} iterations` : ""}
                  {typeof run.actual.durationMs === "number" ? ` · ${Math.round(run.actual.durationMs / 1000)}s` : ""}
                  {typeof run.actual.costUsd === "number" ? ` · ${dollars(run.actual.costUsd)}` : ""}
                </div>
                {run.actual.errorMessage ? (
                  <div style={{ ...styles.thoughtText, color: COLORS.red, marginBottom: 8 }}>
                    error: {run.actual.errorMessage}
                  </div>
                ) : null}
                {run.status === "fail" || run.status === "error" ? (
                  <DiagnosisPanel
                    errorText={
                      run.actual.errorMessage ||
                      run.grades
                        .filter((g) => !g.pass && g.notes)
                        .map((g) => g.notes)
                        .join("\n") ||
                      run.actual.finalText
                    }
                  />
                ) : null}
                {run.actual.finalText ? (
                  <RichText text={run.actual.finalText} />
                ) : (
                  <p style={styles.empty}>No final text.</p>
                )}
              </section>

              <section style={{ ...styles.section, marginTop: 16 }}>
                <h2 style={styles.sectionTitle}>
                  Tool calls <span style={styles.countPill}>{run.actual.toolCalls.length}</span>
                </h2>
                {run.actual.toolCalls.length === 0 ? (
                  <p style={styles.empty}>No tool calls.</p>
                ) : (
                  <div style={styles.thoughtList}>
                    {run.actual.toolCalls.map((c, idx) => (
                      <div key={idx} style={styles.thoughtRow}>
                        <div style={styles.thoughtKind}>#{idx + 1}</div>
                        <div style={{ ...styles.thoughtText, whiteSpace: "normal" }}>
                          <ToolCallLine call={c} />
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </section>

              {run.actual.artifactPaths.length > 0 ? (
                <section style={{ ...styles.section, marginTop: 16 }}>
                  <h2 style={styles.sectionTitle}>
                    Artifacts <span style={styles.countPill}>{run.actual.artifactPaths.length}</span>
                  </h2>
                  <div style={styles.thoughtText}>
                    {run.actual.artifactPaths.join("\n")}
                  </div>
                </section>
              ) : null}
            </>
          ) : (
            <p style={styles.empty}>Eval run not found.</p>
          )}
        </div>
      </AppShell>
    );
  }

  // --- Suite view ----------------------------------------------------------
  if (sp.suite) {
    const runs = await listRuns({ suite: sp.suite, limit: 100 });
    return (
      <AppShell
        active="evals"
        userId={tenant ?? "admin"}
        workspaceName={workspaceLabel(tenant ?? "admin")}
        showHero={false}
      >
        <div style={styles.wrap}>
          <div style={styles.topNav}>
            <Link href={navLink(tenant, "")} style={styles.detailBack}>
              ← All evals
            </Link>
          </div>
          <h1 style={styles.h1}>Suite: {sp.suite}</h1>
          <p style={styles.sub}>Recent runs for this suite. Click a run to inspect graders + output.</p>
          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Runs <span style={styles.countPill}>{runs.length}</span>
            </h2>
            {runs.length === 0 ? (
              <p style={styles.empty}>No runs recorded for this suite.</p>
            ) : (
              <div>
                {runs.map((r, idx) => (
                  <RunRow key={r.id} run={r} tenant={tenant} last={idx === runs.length - 1} />
                ))}
              </div>
            )}
          </section>
        </div>
      </AppShell>
    );
  }

  // --- Overview ------------------------------------------------------------
  const suites = await listSuites();
  const summaries: SuiteSummary[] = (
    await Promise.all(suites.map((s) => suiteSummary(s, 50)))
  ).sort((a, b) => a.passRate - b.passRate);

  // Recent runs across all suites: pull a slice per suite, merge, sort by ts.
  const perSuiteRuns = await Promise.all(
    suites.map((s) => listRuns({ suite: s, limit: 15 }))
  );
  const recentRuns = perSuiteRuns
    .flat()
    .sort((a, b) => b.ts - a.ts)
    .slice(0, 40);

  return (
    <AppShell
      active="evals"
      userId={tenant ?? "admin"}
      workspaceName={workspaceLabel(tenant ?? "admin")}
      showHero={false}
    >
      <div style={styles.wrap}>
        <h1 style={styles.h1}>Evals</h1>
        <p style={styles.sub}>
          Live quality signal from the <code>nx_evals</code> store — pass rates by
          suite and the most recent runs. Click any run to see its graders, tool
          calls, output, and linked job.
        </p>

        <div style={styles.grid}>
          <section style={styles.section}>
            <h2 style={styles.sectionTitle}>
              Suites <span style={styles.countPill}>{summaries.length}</span>
            </h2>
            {summaries.length === 0 ? (
              <p style={styles.empty}>No eval suites yet. Runs are recorded when /job completes.</p>
            ) : (
              <div style={styles.suiteGrid}>
                {summaries.map((s) => (
                  <Link
                    key={s.suite}
                    href={navLink(tenant, `suite=${encodeURIComponent(s.suite)}`)}
                    style={styles.suiteCard}
                  >
                    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 8 }}>
                      <strong style={{ color: COLORS.text, fontSize: 13 }}>{s.suite}</strong>
                      <span
                        style={badge(
                          s.passRate >= 0.8
                            ? COLORS.green
                            : s.passRate >= 0.5
                              ? COLORS.amber
                              : COLORS.red
                        )}
                      >
                        {pct(s.passRate)}
                      </span>
                    </div>
                    <div style={styles.rowSub}>
                      {s.passCount}✓ · {s.failCount}✗ · {s.partialCount}~ · {s.errorCount}!
                      {" "}of {s.totalRunsRecent}
                    </div>
                    <div style={{ ...styles.rowTime, marginTop: 6 }}>
                      last {fmtTime(s.lastRunTs)}
                    </div>
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
              <p style={styles.empty}>No eval runs yet.</p>
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
