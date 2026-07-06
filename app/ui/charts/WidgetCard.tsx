"use client";

// app/ui/charts/WidgetCard.tsx
//
// One dashboard widget: a titled card with a source chip, an actions menu
// (refresh / recompile / delete) and a chart body chosen by WidgetData.kind.
// Presentational — all mutations are delegated to the parent via callbacks.

import type { CSSProperties } from "react";
import type { WidgetSpec, WidgetData } from "@/app/lib/widgetSpec";
import StatTile from "@/app/ui/charts/StatTile";
import LineChart from "@/app/ui/charts/LineChart";
import BarChart from "@/app/ui/charts/BarChart";
import Donut from "@/app/ui/charts/Donut";
import MiniTable from "@/app/ui/charts/MiniTable";

function timeAgo(ts: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function ChartBody({ data }: { data: WidgetData }) {
  switch (data.kind) {
    case "stat":
      return <StatTile value={data.value ?? "—"} unit={data.unit} delta={data.delta} />;
    case "line":
      return <LineChart series={data.series ?? []} />;
    case "bar":
      return <BarChart series={data.series ?? []} />;
    case "donut":
      return <Donut series={data.series ?? []} />;
    case "table":
      return <MiniTable columns={data.columns} rows={data.rows ?? []} />;
    default:
      return null;
  }
}

export default function WidgetCard({
  spec,
  data,
  busy,
  onRefresh,
  onRecompile,
  onDelete,
}: {
  spec: WidgetSpec;
  data?: WidgetData;
  busy?: boolean;
  onRefresh: () => void;
  onRecompile: () => void;
  onDelete: () => void;
}) {
  const err = data?.error ?? spec.lastError;

  return (
    <div style={S.card}>
      <div style={S.head}>
        <div style={S.titleWrap}>
          <div style={S.title}>{spec.title}</div>
          <span style={S.chip}>{spec.source}</span>
        </div>
        <details style={S.menuWrap}>
          <summary style={S.menuButton} aria-label="Widget actions">
            ⋯
          </summary>
          <div style={S.menu}>
            <button type="button" style={S.menuItem} onClick={onRefresh} disabled={busy}>
              Refresh
            </button>
            <button type="button" style={S.menuItem} onClick={onRecompile} disabled={busy}>
              Recompile
            </button>
            <button type="button" style={{ ...S.menuItem, color: "var(--destructive)" }} onClick={onDelete}>
              Delete
            </button>
          </div>
        </details>
      </div>

      <div style={S.body}>
        {busy && !data ? (
          <div style={S.muted}>Loading…</div>
        ) : err ? (
          <div style={S.error}>{err}</div>
        ) : data ? (
          <ChartBody data={data} />
        ) : (
          <div style={S.muted}>No data yet</div>
        )}
      </div>

      <div style={S.foot}>
        <span>{data ? `Updated ${timeAgo(data.updatedAt)}` : "—"}</span>
        {data?.stale ? <span style={S.stale}>stale</span> : null}
        {busy ? <span style={S.muted}>refreshing…</span> : null}
      </div>
    </div>
  );
}

const S: Record<string, CSSProperties> = {
  card: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    boxShadow: "0 2px 6px rgba(0,0,0,0.04)",
    padding: 16,
    display: "flex",
    flexDirection: "column",
    gap: 12,
    minHeight: 180,
  },
  head: {
    display: "flex",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 8,
  },
  titleWrap: { display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap", minWidth: 0 },
  title: { fontSize: 14, fontWeight: 700, color: "var(--foreground)" },
  chip: {
    fontSize: 9,
    fontWeight: 700,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: "var(--muted-foreground)",
    border: "1px solid var(--border)",
    borderRadius: 999,
    padding: "2px 7px",
  },
  menuWrap: { position: "relative" },
  menuButton: {
    listStyle: "none",
    cursor: "pointer",
    width: 26,
    height: 26,
    borderRadius: 7,
    border: "1px solid var(--border)",
    display: "grid",
    placeItems: "center",
    fontSize: 15,
    color: "var(--muted-foreground)",
    userSelect: "none",
  },
  menu: {
    position: "absolute",
    right: 0,
    marginTop: 6,
    minWidth: 130,
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    boxShadow: "0 10px 25px rgba(0,0,0,0.10)",
    overflow: "hidden",
    zIndex: 30,
    display: "flex",
    flexDirection: "column",
  },
  menuItem: {
    textAlign: "left",
    padding: "9px 11px",
    background: "var(--card)",
    border: 0,
    color: "var(--foreground)",
    fontSize: 12,
    cursor: "pointer",
  },
  body: { flex: 1, display: "flex", alignItems: "center" },
  foot: {
    display: "flex",
    alignItems: "center",
    gap: 10,
    color: "var(--muted-foreground)",
    fontSize: 10,
  },
  stale: {
    color: "var(--status-pending)",
    fontWeight: 600,
  },
  muted: { color: "var(--muted-foreground)", fontSize: 11 },
  error: { color: "var(--destructive)", fontSize: 11, lineHeight: 1.5 },
};
