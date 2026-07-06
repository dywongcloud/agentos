"use client";

// app/ui/RecentActivityLive.tsx
//
// Client-side live activity feed. Polls /api/ui/activity every few seconds
// and re-renders. Reads from REAL sources only (the per-tenant activity log
// merged with webhook deliveries, jobs, and code projects).
//
// No mocks. If the feed is empty for a tenant, the empty state says so —
// it does not invent placeholder rows.

import { useEffect, useRef, useState } from "react";
import Pager from "@/app/ui/shell/Pager";

type Item = {
  ts: number;
  kind: string;
  text: string;
  sub?: string;
  href?: string;
};

function timeAgo(ts: number): string {
  const diff = Math.max(0, Date.now() - ts);
  const m = Math.floor(diff / 60_000);
  if (m < 1) return "just now";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  return `${d}d`;
}

function dotFor(kind: string): string {
  switch (kind) {
    case "trigger":
    case "webhook":
      return "●";
    case "job":
    case "job_status":
      return "◆";
    case "code":
    case "code_status":
      return "▣";
    case "eval":
      return "✓";
    case "command":
      return "/";
    case "memory":
      return "✦";
    case "chat":
      return "💬";
    case "login":
      return "🔑";
    case "browse":
      return "⌖";
    case "system":
      return "·";
    default:
      return "▲";
  }
}

const STYLES = {
  list: {
    display: "flex",
    flexDirection: "column" as const,
    gap: 14,
  },
  item: {
    display: "flex",
    gap: 12,
    alignItems: "flex-start" as const,
  },
  iconWrap: {
    width: 26,
    height: 26,
    borderRadius: 13,
    background: "var(--muted)",
    display: "flex",
    alignItems: "center" as const,
    justifyContent: "center" as const,
    fontSize: 12,
    color: "var(--muted-foreground)",
    flexShrink: 0,
  },
  body: { flex: 1, minWidth: 0 },
  text: {
    fontSize: 13,
    color: "var(--foreground)",
    lineHeight: 1.45,
    wordBreak: "break-word" as const,
  },
  sub: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    marginTop: 2,
    textTransform: "lowercase" as const,
  },
  time: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    flexShrink: 0,
    paddingTop: 4,
  },
  empty: {
    fontSize: 12,
    color: "var(--muted-foreground)",
    padding: "12px 0",
  },
  liveDot: {
    display: "inline-block",
    width: 6,
    height: 6,
    borderRadius: 3,
    background: "var(--status-completed)",
    marginRight: 6,
    verticalAlign: "middle" as const,
  },
  pager: {
    display: "flex",
    alignItems: "center" as const,
    justifyContent: "space-between",
    paddingTop: 14,
    marginTop: 6,
    borderTop: "1px solid var(--border)",
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

export default function RecentActivityLive({
  userId,
  variant = "panel",
  limit = 12,
  pageSize,
}: {
  userId: string;
  variant?: "panel" | "large";
  limit?: number;
  pageSize?: number;
}) {
  const [items, setItems] = useState<Item[]>([]);
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
          `/api/ui/activity?userId=${encodeURIComponent(userId)}&limit=${limit}`,
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
    const id = setInterval(tick, 8000);
    return () => {
      alive = false;
      clearInterval(id);
      if (aborter.current) aborter.current.abort();
    };
  }, [userId, limit]);

  if (loaded && items.length === 0) {
    return (
      <div style={STYLES.empty}>
        <span style={STYLES.liveDot} />
        Listening — no activity yet for this tenant.
      </div>
    );
  }

  const itemPad = variant === "large" ? 14 : 12;
  // Default page size matches the visual density of each variant. The
  // panel variant sits in the overview sidebar so a smaller chunk keeps
  // the page from growing past the surrounding card; the large variant
  // sits on the Logs tab and can show more rows per page.
  const effectivePageSize =
    pageSize ?? (variant === "large" ? 15 : 8);
  const totalPages = Math.max(1, Math.ceil(items.length / effectivePageSize));
  const safePage = Math.min(page, totalPages);
  const start = (safePage - 1) * effectivePageSize;
  const visible = items.slice(start, start + effectivePageSize);

  return (
    <div>
      <div style={{ ...STYLES.list, gap: itemPad }}>
        {visible.map((it, idx) => {
          const row = (
            <>
              <div style={STYLES.iconWrap}>{dotFor(it.kind)}</div>
              <div style={STYLES.body}>
                <div style={STYLES.text}>{it.text}</div>
                {it.sub ? <div style={STYLES.sub}>{it.sub}</div> : null}
              </div>
              <div style={STYLES.time}>{timeAgo(it.ts)}</div>
            </>
          );
          if (it.href) {
            return (
              <a
                key={`${it.ts}-${idx}`}
                href={it.href}
                style={{ ...STYLES.item, textDecoration: "none", color: "inherit" }}
              >
                {row}
              </a>
            );
          }
          return (
            <div key={`${it.ts}-${idx}`} style={STYLES.item}>
              {row}
            </div>
          );
        })}
      </div>
      <Pager
        page={safePage}
        total={totalPages}
        count={items.length}
        pageSize={effectivePageSize}
        onPage={setPage}
      />
    </div>
  );
}
