"use client";

// app/ui/AuditLog.tsx
//
// Activity panel for the dashboard. Polls /api/ui/audit which returns the
// merged per-tenant event stream: tool calls, job/code lifecycle, trigger
// fires (e.g. an AI-explained monday board change), automations, and
// integration/settings state changes. OAuth state churn (integration.*
// drift) is filtered out server-side because it was producing spam.

import { useEffect, useRef, useState } from "react";
import Pager from "@/app/ui/shell/Pager";

type AuditItem = {
  id: string;
  ts: number;
  kind: string;
  summary: string;
  before?: string;
  after?: string;
  meta?: Record<string, unknown>;
};

function timeAgo(ts: number): string {
  const diff = Math.max(0, Date.now() - ts);
  const s = Math.floor(diff / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

// Map audit kind → status color token + label fragment. Mirrors the
// upstream dashboard's badge palette.
function paintForKind(kind: string): { color: string; label: string } {
  if (kind.startsWith("integration.connected") || kind.startsWith("integration.refreshed")) {
    return { color: "var(--status-completed)", label: "integration" };
  }
  if (kind.startsWith("integration.disconnected")) {
    return { color: "var(--status-pending)", label: "integration" };
  }
  if (kind.startsWith("integration.expired") || kind.startsWith("integration.revoked")) {
    return { color: "var(--status-failed)", label: "integration" };
  }
  if (kind.startsWith("trigger.")) {
    return { color: "var(--status-running)", label: "trigger" };
  }
  if (kind.startsWith("settings.")) {
    return { color: "var(--muted-foreground)", label: "settings" };
  }
  // High-impact tool calls (mirrored from the activity stream).
  if (kind === "tool.composio_exec") {
    return { color: "var(--status-running)", label: "composio" };
  }
  if (kind === "tool.vfs_write" || kind === "tool.vfs_shell") {
    return { color: "var(--status-completed)", label: "filesystem" };
  }
  if (kind === "tool.browser" || kind.startsWith("browser.")) {
    return { color: "var(--status-cancelled)", label: "browser" };
  }
  // /code project lifecycle.
  if (kind === "tool.code_dispatch") {
    return { color: "var(--status-pending)", label: "code" };
  }
  if (kind === "tool.code_progress") {
    return { color: "var(--muted-foreground)", label: "code" };
  }
  if (kind === "tool.code_turn_done") {
    return { color: "var(--status-completed)", label: "code" };
  }
  if (kind === "tool.code_turn_failed") {
    return { color: "var(--status-failed)", label: "code" };
  }
  if (kind === "tool.code_push") {
    return { color: "var(--status-running)", label: "code" };
  }
  // /job lifecycle.
  if (kind === "tool.job_dispatch") {
    return { color: "var(--status-pending)", label: "job" };
  }
  if (kind === "tool.job_progress") {
    return { color: "var(--muted-foreground)", label: "job" };
  }
  if (kind === "tool.job_done") {
    return { color: "var(--status-completed)", label: "job" };
  }
  if (kind === "tool.job_failed") {
    return { color: "var(--status-failed)", label: "job" };
  }
  return { color: "var(--muted-foreground)", label: "system" };
}

const S = {
  card: {
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 10,
    overflow: "hidden" as const,
  },
  head: {
    padding: "14px 18px",
    borderBottom: "1px solid var(--border)",
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center" as const,
  },
  title: { fontSize: 14, fontWeight: 600, color: "var(--foreground)", margin: 0 },
  sub: { fontSize: 12, color: "var(--muted-foreground)" },
  list: { display: "flex", flexDirection: "column" as const },
  row: {
    display: "grid",
    gridTemplateColumns: "16px minmax(0, 1fr) auto",
    gap: 12,
    padding: "12px 18px",
    borderBottom: "1px solid var(--border)",
    fontSize: 13,
    alignItems: "start" as const,
  },
  dot: (c: string): React.CSSProperties => ({
    width: 8,
    height: 8,
    borderRadius: 4,
    background: c,
    marginTop: 6,
  }),
  text: {
    color: "var(--foreground)",
    lineHeight: 1.45,
    wordBreak: "break-word" as const,
  },
  meta: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    marginTop: 3,
    fontFamily: "var(--font-mono)",
  },
  time: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    whiteSpace: "nowrap" as const,
    paddingTop: 4,
  },
  empty: {
    padding: "26px 18px",
    fontSize: 13,
    color: "var(--muted-foreground)",
    textAlign: "center" as const,
  },
  liveBadge: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    display: "inline-flex",
    alignItems: "center" as const,
    gap: 6,
  },
  liveDot: {
    width: 6,
    height: 6,
    borderRadius: 3,
    background: "var(--status-completed)",
    display: "inline-block",
  },
  pager: {
    display: "flex",
    alignItems: "center" as const,
    justifyContent: "space-between",
    padding: "10px 18px",
    borderTop: "1px solid var(--border)",
    background: "var(--card)",
    fontSize: 12,
    color: "var(--muted-foreground)",
    gap: 12,
  },
  pagerInfo: {
    fontSize: 11,
  },
  pagerControls: {
    display: "flex",
    alignItems: "center" as const,
    gap: 8,
  },
  pagerBtn: {
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 7,
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 500,
    padding: "5px 10px",
    cursor: "pointer",
  },
  pagerBtnDisabled: {
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 7,
    color: "var(--muted-foreground)",
    fontSize: 11,
    fontWeight: 500,
    padding: "5px 10px",
    cursor: "not-allowed" as const,
    opacity: 0.55,
  },
  pagerPage: {
    fontSize: 11,
    color: "var(--foreground)",
    minWidth: 40,
    textAlign: "center" as const,
    fontFamily: "var(--font-mono)",
  },
};

function changeFragment(it: AuditItem): string | null {
  if (it.before && it.after) return `${it.before} → ${it.after}`;
  if (it.after) return it.after;
  if (it.before) return `was ${it.before}`;
  return null;
}

export default function AuditLog({
  userId,
  limit = 60,
  pageSize = 8,
}: {
  userId: string;
  limit?: number;
  pageSize?: number;
}) {
  const [items, setItems] = useState<AuditItem[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [page, setPage] = useState(1);
  const aborter = useRef<AbortController | null>(null);

  useEffect(() => {
    let alive = true;
    async function tick() {
      if (aborter.current) aborter.current.abort();
      const ac = new AbortController();
      aborter.current = ac;
      try {
        const r = await fetch(
          `/api/ui/audit?userId=${encodeURIComponent(userId)}&limit=${limit}`,
          { signal: ac.signal, cache: "no-store" }
        );
        const j = await r.json();
        if (alive && j?.ok && Array.isArray(j.items)) {
          setItems(j.items);
          setLoaded(true);
        }
      } catch {
        // ignore; next tick will retry
      }
    }
    tick();
    const id = setInterval(tick, 10_000);
    return () => {
      alive = false;
      clearInterval(id);
      if (aborter.current) aborter.current.abort();
    };
  }, [userId, limit]);

  const totalPages = Math.max(1, Math.ceil(items.length / pageSize));
  // Clamp page when items shrink (e.g. retention drop) without breaking the
  // user's current position in the common case.
  const safePage = Math.min(page, totalPages);
  const start = (safePage - 1) * pageSize;
  const visible = items.slice(start, start + pageSize);

  return (
    <div style={S.card}>
      <div style={S.head}>
        <div>
          <h2 style={S.title}>Activity</h2>
          <div style={S.sub}>
            Tool calls, jobs, trigger fires, automations, and integration
            changes — what the agent actually did.
          </div>
        </div>
        <span style={S.liveBadge}>
          <span style={S.liveDot} /> live
        </span>
      </div>
      {loaded && items.length === 0 ? (
        <div style={S.empty}>
          No activity yet. Run a tool, fire a trigger, or dispatch a job to
          see entries here.
        </div>
      ) : (
        <>
          <div style={S.list}>
            {visible.map((it) => {
              const paint = paintForKind(it.kind);
              const change = changeFragment(it);
              return (
                <div key={it.id} style={S.row}>
                  <span style={S.dot(paint.color)} />
                  <div>
                    <div style={S.text}>
                      <span
                        style={{
                          fontFamily: "var(--font-mono)",
                          fontSize: 11,
                          color: "var(--muted-foreground)",
                          marginRight: 8,
                        }}
                      >
                        {paint.label}
                      </span>
                      {it.summary}
                    </div>
                    {change ? <div style={S.meta}>{change}</div> : null}
                  </div>
                  <div style={S.time}>{timeAgo(it.ts)}</div>
                </div>
              );
            })}
          </div>
          <Pager
            page={safePage}
            total={totalPages}
            count={items.length}
            pageSize={pageSize}
            onPage={setPage}
          />
        </>
      )}
    </div>
  );
}
