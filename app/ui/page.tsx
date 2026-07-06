// page.tsx
import type { CSSProperties } from "react";
import { headers } from "next/headers";
import { redirect } from "next/navigation";
import Link from "next/link";
import { requireUiAuthPage, uiPathWithQuery } from "@/app/lib/uiRequire";
import { env } from "@/app/lib/env";
import { getGatewayAuthStatus, ensurePairingCode } from "@/app/lib/gatewayAuth";
// import { getTextbeltReplyWebhookUrl } from "@/app/lib/providers/textbelt";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import { getPrimary, getIntervalSeconds, isAutopilotEnabled } from "@/app/lib/autopilotState";

import { Composio } from "@composio/core";
import { VercelProvider } from "@composio/vercel";

import RecentActivityLive from "@/app/ui/RecentActivityLive";
import AuditLog from "@/app/ui/AuditLog";
import VfsBrowser from "@/app/ui/VfsBrowser";
import { toolkitLogo } from "@/app/ui/toolkitLogo";
import AppShell from "@/app/ui/shell/AppShell";
import IntegrationsTable from "@/app/ui/IntegrationsTable";

export const dynamic = "force-dynamic";

type SearchParams = {
  userId?: string;
  tab?: string;
  q?: string;
};

type TabKey =
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

type ToolkitItem = {
  slug: string;
  name?: string;
  connected: boolean;
  connectedAccountId?: string;
};

async function baseUrlFromHeaders(): Promise<string> {
  const h = await headers();
  const proto = h.get("x-forwarded-proto") ?? "https";
  const host = h.get("x-forwarded-host") ?? h.get("host") ?? "localhost:3000";
  return `${proto}://${host}`;
}

function formatToolkitName(slug: string, name?: string) {
  if (name && name.trim()) return name;
  return slug
    .replace(/[-_]/g, " ")
    .replace(/\b\w/g, (m) => m.toUpperCase());
}

function firstLetter(value: string) {
  const clean = value.trim();
  return clean ? clean[0]!.toUpperCase() : "D";
}

function initialsFromSlug(slug: string) {
  const parts = slug.split(/[-_\s]+/).filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase();
  return `${parts[0]![0] ?? ""}${parts[1]![0] ?? ""}`.toUpperCase();
}

function navHref(tab: TabKey, userId: string, q?: string) {
  const params = new URLSearchParams();
  params.set("tab", tab);
  params.set("userId", userId);
  if (q) params.set("q", q);
  return `/ui?${params.toString()}`;
}

// (toolkitLogo moved to app/ui/toolkitLogo.ts — shared with the agents canvas.)

// (Mock activity helpers removed — the dashboard now sources activity from
// the real per-tenant activity log + webhook ring + job/code project state
// via the RecentActivityLive client component.)

export default async function UiPage({
  searchParams,
}: {
  searchParams?: Promise<SearchParams>;
}) {
  const sp = (await searchParams) ?? {};
  await requireUiAuthPage(uiPathWithQuery("/ui", sp));

  const autopilotEnabled = await isAutopilotEnabled();
  const primary = await getPrimary();
  const intervalSeconds = await getIntervalSeconds();

  const userId: string = (await resolveUiTenant(sp.userId)) ?? "admin";

  const activeTab: TabKey = (() => {
    const raw = sp.tab;
    if (
      raw === "overview" ||
      raw === "files" ||
      // backwards-compat aliases for old bookmarks: both `services` (the
      // previous tab name) and `skills-deployments` (the one before that)
      // resolve to the new Files tab.
      raw === "services" ||
      raw === "skills-deployments" ||
      raw === "integrations" ||
      raw === "workflows" ||
      raw === "evals" ||
      raw === "automations" ||
      raw === "agents" ||
      raw === "logs" ||
      raw === "activity" ||
      raw === "domains" ||
      raw === "usage" ||
      raw === "settings"
    ) {
      if (raw === "skills-deployments" || raw === "services") return "files";
      return raw;
    }
    return "overview";
  })();

  const searchQuery = sp.q?.trim().toLowerCase() ?? "";

  const baseUrlRaw = env("APP_BASE_URL") ?? (await baseUrlFromHeaders());
  const normalizedBase = baseUrlRaw.replace(/\/$/, "");

  const gateway = await getGatewayAuthStatus();
  const pairing = gateway.paired ? null : await ensurePairingCode();
  const pairingCode = gateway.paired ? undefined : pairing?.code ?? gateway.pairingCode;

  let composioToolkits: ToolkitItem[] = [];
  let composioError: string | null = null;

  if (env("COMPOSIO_API_KEY")) {
    try {
      const composio = new Composio({ provider: new VercelProvider() });
      const session: any = await composio.create(userId, { manageConnections: false });
      const toolkits: any = await session.toolkits();
      const items = toolkits?.items ?? toolkits?.toolkits ?? [];
      composioToolkits = (items as any[]).map((t) => {
        const slug = t.slug ?? t.name ?? "unknown";
        const connectedAccountId =
          t.connection?.connectedAccount?.id ?? t.connection?.connected_account?.id;
        const connected =
          !!connectedAccountId || !!t.connection?.isActive || !!t.connection?.is_active;
        return { slug, name: t.name, connected, connectedAccountId };
      });

      composioToolkits.sort((a, b) => {
        if (a.connected !== b.connected) return a.connected ? -1 : 1;
        return a.slug.localeCompare(b.slug);
      });
    } catch (e: any) {
      composioError = e?.message ?? String(e);
    }
  } else {
    composioError = "COMPOSIO_API_KEY is not set.";
  }

  const filteredToolkits = composioToolkits.filter((t) => {
    if (!searchQuery) return true;
    const hay = `${t.slug} ${t.name ?? ""}`.toLowerCase();
    return hay.includes(searchQuery);
  });

  const visibleCards = filteredToolkits.slice(0, 4);
  const connectedCount = composioToolkits.filter((t) => t.connected).length;
  const userLabel = userId.includes(":") ? userId.split(":")[1] || userId : userId;
  const profileName = userLabel === "admin" ? "Admin" : userLabel;

  const telegramWebhookUrl = `${normalizedBase}/telegram`;
  const whatsappWebhookUrl = `${normalizedBase}/whatsapp`;
  const smsWebhookUrl = `${normalizedBase}/sms`;

  return (
    <AppShell
      active={activeTab}
      userId={userId}
      workspaceName={profileName}
      q={searchQuery}
      showHero={false}
    >
          <div style={styles.heroRow}>
            <div style={styles.heroLeft}>
              <div style={styles.bigAvatar}>{firstLetter(profileName)}</div>
              <div>
                <h1 style={styles.pageTitle}>
                  {profileName}&apos;s{" "}
                  {activeTab === "files" ? "Files" : "Services"}
                </h1>
                <div style={styles.subline}>
                  <span style={styles.gitIcon}>⌘</span>
                  <span>
                    {activeTab === "files" ? (
                      <>
                        Virtual file system <span style={styles.slashInline}>/</span>{" "}
                        <Link
                          href={navHref("integrations", userId, searchQuery)}
                          style={styles.inlineBlue}
                        >
                          Integrations
                        </Link>
                      </>
                    ) : (
                      <>
                        Connected to Composio <span style={styles.slashInline}>/</span>{" "}
                        <Link
                          href={navHref("settings", userId, searchQuery)}
                          style={styles.inlineBlue}
                        >
                          Settings
                        </Link>
                      </>
                    )}
                  </span>
                </div>
              </div>
            </div>

            <div>
              <details>
                <summary style={styles.primaryAction}>
                  {activeTab === "files" ? "View options ▾" : "New Service ▾"}
                </summary>
                <div style={styles.menuPopover}>
                  <Link href={navHref("integrations", userId, searchQuery)} style={styles.menuItem}>
                    View integrations
                  </Link>
                  <Link href={navHref("settings", userId, searchQuery)} style={styles.menuItem}>
                    Open settings
                  </Link>
                </div>
              </details>
            </div>
          </div>

          {activeTab !== "files" && (
            <div style={styles.searchRow}>
              <form method="get" action="/ui" style={styles.searchForm}>
                <input type="hidden" name="tab" value={activeTab} />
                <input type="hidden" name="userId" value={userId} />
                <div style={styles.searchWrap}>
                  <span style={styles.searchIcon}>⌕</span>
                  <input
                    name="q"
                    defaultValue={sp.q ?? ""}
                    placeholder="Search..."
                    style={styles.searchInput}
                  />
                </div>
                <button type="submit" style={styles.newProjectButton}>
                  New Service
                </button>
                <Link href={navHref("settings", userId, searchQuery)} style={styles.iconButton}>
                  ⍟
                </Link>
              </form>
            </div>
          )}

          {activeTab === "overview" && (
            <div style={styles.mainGrid}>
              <div style={styles.cardsArea}>
                {composioError ? (
                  <div style={styles.errorCard}>{composioError}</div>
                ) : visibleCards.length === 0 ? (
                  <div style={styles.errorCard}>No integrations matched your search.</div>
                ) : (
                  visibleCards.map((toolkit, index) => {
                    const logo = toolkitLogo(toolkit.slug);
                    const displayName = formatToolkitName(toolkit.slug, toolkit.name);
                    return (
                      <article key={toolkit.slug} style={styles.projectCard}>
                        <div style={styles.cardHead}>
                          <div style={styles.cardIdentity}>
                            <div style={styles.logoBadge}>
                              {logo ? (
                                <img src={logo} alt={displayName} style={styles.logoImage} />
                              ) : (
                                <span style={styles.logoFallback}>
                                  {initialsFromSlug(toolkit.slug)}
                                </span>
                              )}
                            </div>

                            <div>
                              <div style={styles.cardTitleRow}>
                                <div style={styles.cardTitle}>{displayName}</div>
                                {toolkit.connected ? (
                                  <span style={styles.healthPill}>100</span>
                                ) : (
                                  <span style={styles.healthPillMuted}>—</span>
                                )}
                              </div>
                              <div style={styles.cardDomain}>
                                {toolkit.connected
                                  ? `${toolkit.slug}.connected`
                                  : `${toolkit.slug}.not-connected`}
                              </div>
                            </div>
                          </div>
                        </div>

                        <p style={styles.cardDescription}>
                          {toolkit.connected
                            ? `${displayName} is authorized and ready for agent use through Composio.`
                            : `${displayName} is available but still needs authorization before the agent can use it.`}
                        </p>

                        <div style={styles.cardFooter}>
                          {toolkit.connected
                            ? `connected via Composio`
                            : `pending via Composio`}
                        </div>
                      </article>
                    );
                  })
                )}
              </div>

              <aside style={styles.activityPanel}>
                <h2 style={styles.activityTitle}>Audit log</h2>

                {/* State-changing entries only — Composio integration
                    connect/disconnect/expire, trigger sub/unsub, settings
                    flips. The event stream lives in the Logs tab. */}
                <AuditLog userId={userId} limit={12} />
              </aside>
            </div>
          )}

          {activeTab === "files" && (
            <section>
              <VfsBrowser userId={userId} />
            </section>
          )}

          {activeTab === "integrations" && (
            <section style={styles.panelCard}>
              <h2 style={styles.sectionTitle}>All Integrations</h2>
              <IntegrationsTable
                userId={userId}
                toolkits={filteredToolkits}
                error={composioError}
              />
            </section>
          )}

          {activeTab === "workflows" && (() => {
            // Workflows tab serves the literal upstream Vercel Workflow
            // DevKit dashboard. It's mounted at our app's root (see the
            // rewrites in next.config.ts and the bridge in
            // app/wf-app/[[...slug]]/route.ts). At root, the upstream React
            // Router build matches its `/`, `/run/:id` etc. routes natively
            // — no basename needed.
            redirect(`/`);
          })()}

          {activeTab === "evals" && (() => {
            // Evals get their own server-rendered surface (suite cards, run
            // detail, grader breakdown) at /ui/evals, the same way deep jobs
            // live under /ui/workflows. Redirect there preserving the tenant.
            redirect(
              `/ui/evals${userId ? `?userId=${encodeURIComponent(userId)}` : ""}`
            );
          })()}

          {activeTab === "automations" && (() => {
            // Automations ("flows") get their own server-rendered surface
            // (rule list, run history, run detail) at /ui/automations, the
            // same way deep jobs live under /ui/workflows. Redirect there
            // preserving the tenant.
            redirect(
              `/ui/automations${userId ? `?userId=${encodeURIComponent(userId)}` : ""}`
            );
          })()}

          {activeTab === "agents" && (() => {
            // Agents/workforces get their own server-rendered surface (the
            // ReactFlow workforce canvas) at /ui/agents, the same way
            // automations live under /ui/automations. Redirect there
            // preserving the tenant.
            redirect(
              `/ui/agents${userId ? `?userId=${encodeURIComponent(userId)}` : ""}`
            );
          })()}

          {activeTab === "logs" && (
            <section style={styles.panelCard}>
              <h2 style={styles.sectionTitle}>Logs</h2>
              {/* Meta view — the per-tenant activity feed merged across the
                  activity log, webhook deliveries, and workflow status
                  changes. Polls /api/ui/activity every few seconds. */}
              <div style={styles.activityListLarge}>
                <RecentActivityLive userId={userId} variant="large" limit={80} />
              </div>
            </section>
          )}

          {activeTab === "activity" && (
            <section>
              {/* Audit log only — Composio integration changes, trigger
                  sub/unsub, settings flips. The event stream (job dispatches,
                  trigger fires, tool calls) lives in the Logs tab. */}
              <AuditLog userId={userId} limit={120} />
            </section>
          )}

          {activeTab === "domains" && (
            <section style={styles.panelCard}>
              <h2 style={styles.sectionTitle}>Endpoints</h2>
              <ul style={styles.infoList}>
                <li>
                  <code>{normalizedBase}/health</code>
                </li>
                <li>
                  <code>{telegramWebhookUrl}</code>
                </li>
                <li>
                  <code>{whatsappWebhookUrl}</code>
                </li>
                <li>
                  <code>{smsWebhookUrl}</code>
                </li>
                <li>
                  <code>{normalizedBase}/pair</code>
                </li>
                <li>
                  <code>{normalizedBase}/webhook</code>
                </li>
              </ul>
            </section>
          )}

          {activeTab === "usage" && (
            <section style={styles.panelCard}>
              <h2 style={styles.sectionTitle}>Usage</h2>
              <div style={styles.usageGrid}>
                <div style={styles.metricCard}>
                  <div style={styles.metricLabel}>Connected Integrations</div>
                  <div style={styles.metricValue}>{connectedCount}</div>
                </div>
                <div style={styles.metricCard}>
                  <div style={styles.metricLabel}>Available Toolkits</div>
                  <div style={styles.metricValue}>{composioToolkits.length}</div>
                </div>
                <div style={styles.metricCard}>
                  <div style={styles.metricLabel}>Autopilot</div>
                  <div style={styles.metricValueSmall}>
                    {autopilotEnabled ? "Enabled" : "Disabled"}
                  </div>
                </div>
                <div style={styles.metricCard}>
                  <div style={styles.metricLabel}>Interval</div>
                  <div style={styles.metricValueSmall}>{intervalSeconds}s</div>
                </div>
              </div>
            </section>
          )}

          {activeTab === "settings" && (
            <section style={styles.settingsGrid}>
              <div style={styles.panelCard}>
                <h2 style={styles.sectionTitle}>Identity</h2>
                <form method="get" action="/ui" style={styles.formStack}>
                  <input type="hidden" name="tab" value="overview" />
                  <label style={styles.label}>
                    <span>Composio userId</span>
                    <input
                      name="userId"
                      defaultValue={userId}
                      placeholder="telegram:123456789"
                      style={styles.input}
                    />
                  </label>
                  <button type="submit" style={styles.submitButton}>
                    Load
                  </button>
                </form>
                <form
                  method="post"
                  action="/api/ui/identity/set-last"
                  style={styles.formStack}
                >
                  <input type="hidden" name="userId" value={userId} />
                  <button type="submit" style={styles.submitButtonAlt}>
                    Pin as dashboard default
                  </button>
                </form>
              </div>

              <div style={styles.panelCard}>
                <h2 style={styles.sectionTitle}>Gateway</h2>
                <p style={styles.sectionText}>
                  Pair URL: <code>{normalizedBase}/pair</code>
                  <br />
                  Webhook URL: <code>{normalizedBase}/webhook</code>
                </p>
                <p style={styles.sectionText}>
                  {gateway.paired ? (
                    <>Gateway is paired.</>
                  ) : (
                    <>
                      Gateway is not paired.
                      <br />
                      Pairing code: <code>{pairingCode ?? "(not generated yet)"}</code>
                    </>
                  )}
                </p>
                <div style={styles.actionRow}>
                  <form action="/api/ui/gateway/regenerate-pairing" method="post">
                    <button type="submit" style={styles.submitButtonAlt}>
                      Regenerate pairing code
                    </button>
                  </form>
                  <form action="/api/ui/gateway/clear-token" method="post">
                    <button type="submit" style={styles.submitButton}>
                      Clear bearer token
                    </button>
                  </form>
                </div>
              </div>

              <div style={styles.panelCard}>
                <h2 style={styles.sectionTitle}>Channels</h2>
                <div style={styles.actionRow}>
                  <form action="/api/ui/telegram/set-webhook" method="post">
                    <button type="submit" style={styles.submitButton}>
                      Set Telegram webhook
                    </button>
                  </form>
                  <form action="/api/ui/telegram/delete-webhook" method="post">
                    <button type="submit" style={styles.submitButtonAlt}>
                      Delete Telegram webhook
                    </button>
                  </form>
                </div>
                <div style={{ ...styles.actionRow, marginTop: 8 }}>
                  <form action="/api/ui/autopilot/start" method="post">
                    <button type="submit" style={styles.submitButton}>
                      Start Autopilot
                    </button>
                  </form>
                  <form action="/api/ui/autopilot/stop" method="post">
                    <button type="submit" style={styles.submitButtonAlt}>
                      Stop Autopilot
                    </button>
                  </form>
                </div>
                <p style={{ ...styles.sectionText, marginTop: 11 }}>
                  Primary destination:{" "}
                  <code>{primary ? `${primary.channel} / ${primary.sessionId}` : "(not set yet)"}</code>
                </p>
              </div>
            </section>
          )}
    </AppShell>
  );
}

const styles: Record<string, CSSProperties> = {
  page: {
    minHeight: "100vh",
    background: "var(--background)",
    padding: 0,
    margin: 0,
    color: "var(--foreground)",
    fontFamily:
      'Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif',
  },

  app: {
    minHeight: "100vh",
    background: "var(--card)",
  },

  topbar: {
    height: 62,
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "0 23px 0 93px",
    borderBottom: "1px solid var(--border)",
    gap: 12,
    flexWrap: "wrap",
  },

  topbarLeft: {
    display: "flex",
    alignItems: "center",
    gap: 12,
  },

  brandMark: {
    width: 18,
    height: 18,
    display: "grid",
    placeItems: "center",
  },

  slash: {
    color: "var(--muted-foreground)",
    fontSize: 30,
    lineHeight: 1,
    fontWeight: 200,
    marginTop: -3,
  },

  workspaceName: {
    fontSize: 25,
    fontWeight: 500,
    lineHeight: 1,
    letterSpacing: "-0.02em",
  },

  topbarRight: {
    display: "flex",
    alignItems: "center",
    gap: 14,
    marginLeft: "auto",
  },

  feedbackButton: {
    textDecoration: "none",
    color: "var(--foreground)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    padding: "9px 14px",
    fontSize: 11,
    fontWeight: 500,
    background: "var(--card)",
  },

  toplink: {
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 500,
  },

  topDots: {
    color: "var(--foreground)",
    fontSize: 14,
    lineHeight: 1,
    letterSpacing: 1,
  },

  avatarCircle: {
    width: 29,
    height: 29,
    borderRadius: 999,
    background: "var(--primary)",
    color: "var(--card)",
    display: "grid",
    placeItems: "center",
    fontSize: 15,
    fontWeight: 500,
  },

  // Pill-style tab bar to mirror the upstream Workflow dashboard. A subtle
  // muted background holds the whole row; the active tab gets an elevated
  // card background with a ring instead of an underline.
  tabbar: {
    display: "flex",
    alignItems: "center",
    gap: 4,
    margin: "16px 93px 0 93px",
    padding: 4,
    background: "var(--muted)",
    borderRadius: 10,
    width: "fit-content",
    overflowX: "auto",
  },

  mainTab: {
    textDecoration: "none",
    color: "var(--muted-foreground)",
    fontSize: 13,
    fontWeight: 500,
    padding: "6px 14px",
    borderRadius: 7,
    whiteSpace: "nowrap",
    transition: "color 120ms ease, background 120ms ease",
  },

  mainTabActive: {
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 13,
    fontWeight: 600,
    padding: "6px 14px",
    borderRadius: 7,
    background: "var(--card)",
    boxShadow: "0 1px 2px 0 rgba(0,0,0,0.18)",
    whiteSpace: "nowrap",
  },

  content: {
    padding: "29px 114px 45px 114px",
  },

  heroRow: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 15,
    marginBottom: 26,
    flexWrap: "wrap",
  },

  heroLeft: {
    display: "flex",
    alignItems: "center",
    gap: 15,
  },

  bigAvatar: {
    width: 57,
    height: 57,
    borderRadius: 999,
    background: "var(--primary)",
    color: "var(--card)",
    display: "grid",
    placeItems: "center",
    fontSize: 36,
    fontWeight: 400,
  },

  pageTitle: {
    margin: 0,
    fontSize: 23,
    lineHeight: 1.15,
    letterSpacing: "-0.03em",
    fontWeight: 650,
  },

  subline: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    marginTop: 6,
    color: "var(--muted-foreground)",
    fontSize: 11,
  },

  gitIcon: {
    fontSize: 12,
    opacity: 0.8,
  },

  slashInline: {
    color: "var(--muted-foreground)",
    margin: "0 3px",
  },

  inlineBlue: {
    color: "var(--status-running)",
    textDecoration: "none",
    fontWeight: 500,
  },

  primaryAction: {
    listStyle: "none",
    cursor: "pointer",
    background: "var(--foreground)",
    color: "var(--card)",
    borderRadius: 9,
    padding: "12px 14px",
    fontSize: 11,
    fontWeight: 600,
    minWidth: 120,
    textAlign: "center",
    userSelect: "none",
  },

  menuPopover: {
    position: "absolute",
    marginTop: 6,
    minWidth: 135,
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    boxShadow: "0 10px 25px rgba(0,0,0,0.08)",
    overflow: "hidden",
    zIndex: 20,
  },

  menuItem: {
    display: "block",
    padding: "9px 11px",
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 11,
  },

  searchRow: {
    marginBottom: 21,
  },

  searchForm: {
    display: "grid",
    gridTemplateColumns: "1fr 113px 44px",
    gap: 14,
    alignItems: "center",
  },

  searchWrap: {
    display: "flex",
    alignItems: "center",
    border: "1px solid var(--border)",
    borderRadius: 11,
    height: 44,
    padding: "0 12px",
    background: "var(--card)",
    boxShadow: "0 1px 2px rgba(0,0,0,0.03)",
  },

  searchIcon: {
    color: "var(--muted-foreground)",
    marginRight: 8,
    fontSize: 17,
    lineHeight: 1,
  },

  searchInput: {
    width: "100%",
    height: "100%",
    border: 0,
    outline: "none",
    fontSize: 13,
    color: "var(--foreground)",
    background: "transparent",
  },

  newProjectButton: {
    height: 44,
    border: 0,
    borderRadius: 11,
    background: "var(--foreground)",
    color: "var(--card)",
    fontSize: 12,
    fontWeight: 500,
    cursor: "pointer",
  },

  iconButton: {
    height: 44,
    borderRadius: 11,
    border: "1px solid var(--border)",
    display: "grid",
    placeItems: "center",
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 17,
    background: "var(--card)",
    boxShadow: "0 1px 2px rgba(0,0,0,0.03)",
  },

  mainGrid: {
    display: "grid",
    gridTemplateColumns: "minmax(0, 1fr) 282px",
    gap: 26,
    alignItems: "start",
  },

  cardsArea: {
    display: "grid",
    gridTemplateColumns: "repeat(2, minmax(0, 1fr))",
    gap: 15,
  },

  projectCard: {
    minHeight: 195,
    borderRadius: 14,
    border: "1px solid var(--border)",
    background: "var(--card)",
    boxShadow: "0 2px 6px rgba(0,0,0,0.04)",
    padding: "24px 20px 20px 20px",
    display: "flex",
    flexDirection: "column",
  },

  cardHead: {
    marginBottom: 17,
  },

  cardIdentity: {
    display: "flex",
    alignItems: "center",
    gap: 14,
  },

  logoBadge: {
    width: 33,
    height: 33,
    borderRadius: 999,
    background: "var(--card)",
    border: "1px solid var(--border)",
    display: "grid",
    placeItems: "center",
    overflow: "hidden",
    flexShrink: 0,
  },

  logoImage: {
    width: 18,
    height: 18,
    objectFit: "contain",
    display: "block",
  },

  logoFallback: {
    fontSize: 9,
    fontWeight: 700,
    color: "var(--foreground)",
    letterSpacing: "0.04em",
  },

  cardTitleRow: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    marginBottom: 2,
    flexWrap: "wrap",
  },

  cardTitle: {
    fontSize: 14,
    fontWeight: 700,
    lineHeight: 1.15,
    color: "var(--foreground)",
  },

  healthPill: {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    minWidth: 27,
    height: 23,
    padding: "0 6px",
    borderRadius: 999,
    border: "2px solid var(--status-completed)",
    color: "var(--status-completed)",
    fontWeight: 700,
    fontSize: 11,
    lineHeight: 1,
  },

  healthPillMuted: {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    minWidth: 27,
    height: 23,
    padding: "0 6px",
    borderRadius: 999,
    border: "2px solid var(--border)",
    color: "var(--muted-foreground)",
    fontWeight: 700,
    fontSize: 11,
    lineHeight: 1,
  },

  cardDomain: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    lineHeight: 1.2,
  },

  cardDescription: {
    margin: "6px 0 0 0",
    color: "var(--muted-foreground)",
    fontSize: 13,
    lineHeight: 1.4,
    maxWidth: 500,
  },

  cardFooter: {
    marginTop: "auto",
    paddingTop: 14,
    color: "var(--muted-foreground)",
    fontSize: 11,
  },

  cardActionLink: {
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 600,
    whiteSpace: "nowrap",
  },

  activityPanel: {
    background: "transparent",
  },

  activityTitle: {
    margin: "0 0 14px 0",
    fontSize: 17,
    lineHeight: 1.2,
    fontWeight: 700,
    color: "var(--foreground)",
  },

  activityList: {
    display: "flex",
    flexDirection: "column",
    gap: 14,
  },

  activityItem: {
    display: "grid",
    gridTemplateColumns: "38px 1fr auto",
    gap: 9,
    alignItems: "start",
  },

  activityIconWrap: {
    width: 36,
    height: 36,
    borderRadius: 999,
    background: "var(--muted)",
    border: "1px solid var(--border)",
    display: "grid",
    placeItems: "center",
    boxShadow: "inset 0 0 0 2px var(--muted)",
  },

  activityIcon: {
    color: "var(--muted-foreground)",
    fontSize: 11,
    lineHeight: 1,
  },

  activityBody: {
    minWidth: 0,
    paddingTop: 3,
  },

  activityText: {
    color: "var(--foreground)",
    fontSize: 11,
    lineHeight: 1.35,
  },

  activitySub: {
    color: "var(--muted-foreground)",
    fontSize: 11,
    marginTop: 2,
    lineHeight: 1.3,
  },

  activityTime: {
    color: "var(--muted-foreground)",
    fontSize: 11,
    whiteSpace: "nowrap",
    paddingTop: 3,
  },

  activityEmpty: {
    color: "var(--muted-foreground)",
    fontSize: 11,
  },

  panelCard: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    boxShadow: "0 2px 6px rgba(0,0,0,0.04)",
    padding: 18,
  },

  sectionTitle: {
    margin: "0 0 11px 0",
    fontSize: 17,
    fontWeight: 700,
    color: "var(--foreground)",
  },

  sectionText: {
    margin: 0,
    color: "var(--muted-foreground)",
    fontSize: 11,
    lineHeight: 1.6,
  },

  tableWrap: {
    overflowX: "auto",
  },

  table: {
    width: "100%",
    borderCollapse: "collapse",
  },

  th: {
    textAlign: "left",
    padding: "9px 9px",
    fontSize: 9,
    color: "var(--muted-foreground)",
    fontWeight: 600,
    borderBottom: "1px solid var(--border)",
    whiteSpace: "nowrap",
  },

  td: {
    padding: "11px 9px",
    fontSize: 11,
    color: "var(--foreground)",
    borderBottom: "1px solid var(--border)",
    whiteSpace: "nowrap",
    verticalAlign: "middle",
  },

  tdStrong: {
    padding: "11px 9px",
    fontSize: 11,
    color: "var(--foreground)",
    borderBottom: "1px solid var(--border)",
    whiteSpace: "nowrap",
    verticalAlign: "middle",
    fontWeight: 600,
  },

  tableLink: {
    textDecoration: "none",
    color: "var(--status-running)",
    fontWeight: 600,
  },

  integrationCell: {
    display: "flex",
    alignItems: "center",
    gap: 8,
  },

  smallLogoBadge: {
    width: 21,
    height: 21,
    borderRadius: 999,
    background: "var(--card)",
    border: "1px solid var(--border)",
    display: "grid",
    placeItems: "center",
    overflow: "hidden",
    flexShrink: 0,
  },

  smallLogoImage: {
    width: 12,
    height: 12,
    objectFit: "contain",
    display: "block",
  },

  smallLogoFallback: {
    fontSize: 8,
    fontWeight: 700,
    color: "var(--foreground)",
    letterSpacing: "0.04em",
  },

  activityListLarge: {
    display: "flex",
    flexDirection: "column",
    gap: 12,
  },

  activityItemLarge: {
    display: "grid",
    gridTemplateColumns: "38px 1fr auto",
    gap: 9,
    alignItems: "start",
    paddingBottom: 9,
    borderBottom: "1px solid var(--border)",
  },

  activityBodyLarge: {
    minWidth: 0,
  },

  infoList: {
    margin: 0,
    paddingLeft: 14,
    color: "var(--foreground)",
    fontSize: 11,
    lineHeight: 1.8,
  },

  usageGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(4, minmax(0, 1fr))",
    gap: 14,
  },

  metricCard: {
    border: "1px solid var(--border)",
    borderRadius: 12,
    padding: 15,
    background: "var(--card)",
  },

  metricLabel: {
    color: "var(--muted-foreground)",
    fontSize: 10,
    marginBottom: 8,
  },

  metricValue: {
    color: "var(--foreground)",
    fontSize: 26,
    fontWeight: 700,
    lineHeight: 1,
  },

  metricValueSmall: {
    color: "var(--foreground)",
    fontSize: 17,
    fontWeight: 700,
    lineHeight: 1.1,
  },

  settingsGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(3, minmax(0, 1fr))",
    gap: 15,
  },

  formStack: {
    display: "grid",
    gap: 9,
  },

  label: {
    display: "grid",
    gap: 5,
    color: "var(--foreground)",
    fontSize: 10,
  },

  input: {
    width: "100%",
    height: 33,
    borderRadius: 9,
    border: "1px solid var(--border)",
    padding: "0 9px",
    outline: "none",
    fontSize: 11,
    color: "var(--foreground)",
    background: "var(--card)",
  },

  submitButton: {
    height: 33,
    borderRadius: 9,
    border: 0,
    background: "var(--foreground)",
    color: "var(--card)",
    fontSize: 11,
    fontWeight: 600,
    cursor: "pointer",
    padding: "0 11px",
  },

  submitButtonAlt: {
    height: 33,
    borderRadius: 9,
    border: "1px solid var(--border)",
    background: "var(--card)",
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 600,
    cursor: "pointer",
    padding: "0 11px",
  },

  actionRow: {
    display: "flex",
    gap: 8,
    flexWrap: "wrap",
  },

  errorCard: {
    gridColumn: "1 / -1",
    minHeight: 135,
    borderRadius: 14,
    border: "1px solid var(--destructive)",
    background: "var(--card)7f7",
    color: "var(--destructive)",
    padding: 18,
    fontSize: 11,
    lineHeight: 1.5,
  },
};
