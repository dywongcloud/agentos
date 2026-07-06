"use client";

// app/ui/agents/ActivityDashboard.tsx
//
// Real task-activity dashboard for a workforce, fed by GET
// /api/ui/agents?op=activity&teamId=&range=. Four filter tabs (Escalated / To
// review / Errored / All tasks) with live counts, a timeline histogram that
// toggles 24h ⇄ Past week, and a task table (TIME · TASK+agent · COST · SAVED ·
// STATUS). Everything is real jobStore data — empty states show honestly when a
// team has not run anything yet.

import { useCallback, useEffect, useMemo, useState } from "react";
import type { CSSProperties } from "react";

type ActivityStatus =
  | "Error"
  | "Escalated"
  | "To review"
  | "Complete"
  | "Running";

type ActivityTask = {
  jobId: string;
  time: number;
  prompt: string;
  agentId: string | null;
  agentName: string;
  agentEmoji: string;
  cost: number | null;
  durationMs: number | null;
  status: ActivityStatus;
  rawStatus: string;
  escalated: boolean;
};

type ActivityData = {
  tasks: ActivityTask[];
  counts: { escalated: number; toReview: number; errored: number; all: number };
  timeline: Array<{ label: string; count: number }>;
  range: "24h" | "week";
};

type TabKey = "escalated" | "toReview" | "errored" | "all";

const STATUS_STYLE: Record<ActivityStatus, { bg: string; fg: string; dot: string }> = {
  Error: { bg: "rgba(239,68,68,0.12)", fg: "#ef4444", dot: "#ef4444" },
  Escalated: { bg: "rgba(245,158,11,0.14)", fg: "#d97706", dot: "#f59e0b" },
  "To review": { bg: "rgba(99,102,241,0.12)", fg: "#6366f1", dot: "#6366f1" },
  Complete: { bg: "rgba(34,197,94,0.12)", fg: "#16a34a", dot: "#22c55e" },
  Running: { bg: "rgba(148,163,184,0.16)", fg: "#64748b", dot: "#94a3b8" },
};

function fmtTime(ms: number): string {
  const d = new Date(ms);
  const now = Date.now();
  const diff = now - ms;
  if (diff < 60000) return "just now";
  if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
  if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

function fmtCost(c: number | null): string {
  if (c == null) return "—";
  if (c < 0.01) return `$${c.toFixed(4)}`;
  return `$${c.toFixed(2)}`;
}

function fmtDuration(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  return `${Math.floor(s / 60)}m ${Math.round(s % 60)}s`;
}

const card: CSSProperties = {
  background: "var(--card)",
  border: "1px solid var(--border)",
  borderRadius: 12,
  padding: 16,
};

export default function ActivityDashboard({
  userId,
  teamId,
}: {
  userId: string;
  teamId: string;
}) {
  const [data, setData] = useState<ActivityData | null>(null);
  const [tab, setTab] = useState<TabKey>("all");
  const [range, setRange] = useState<"24h" | "week">("24h");
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const res = await fetch(
        `/api/ui/agents?op=activity&teamId=${encodeURIComponent(
          teamId
        )}&range=${range}&userId=${encodeURIComponent(userId)}`,
        { cache: "no-store" }
      );
      if (!res.ok) return;
      setData((await res.json()) as ActivityData);
    } catch {
      // transient — keep last state
    } finally {
      setLoading(false);
    }
  }, [teamId, range, userId]);

  useEffect(() => {
    setLoading(true);
    void load();
    const id = setInterval(() => void load(), 8000);
    return () => clearInterval(id);
  }, [load]);

  const counts = data?.counts ?? { escalated: 0, toReview: 0, errored: 0, all: 0 };

  const filtered = useMemo(() => {
    const all = data?.tasks ?? [];
    switch (tab) {
      case "escalated":
        return all.filter((t) => t.status === "Escalated");
      case "toReview":
        return all.filter((t) => t.status === "To review");
      case "errored":
        return all.filter((t) => t.status === "Error");
      default:
        return all;
    }
  }, [data, tab]);

  const maxBar = useMemo(
    () => Math.max(1, ...(data?.timeline ?? []).map((b) => b.count)),
    [data]
  );

  const tabs: Array<{ key: TabKey; label: string; count: number }> = [
    { key: "escalated", label: "Escalated", count: counts.escalated },
    { key: "toReview", label: "To review", count: counts.toReview },
    { key: "errored", label: "Errored", count: counts.errored },
    { key: "all", label: "All tasks", count: counts.all },
  ];

  return (
    <div
      style={{
        background: "var(--card)",
        border: "1px solid var(--border)",
        borderTop: "none",
        borderRadius: "0 0 12px 12px",
        padding: 24,
        display: "flex",
        flexDirection: "column",
        gap: 18,
      }}
    >
      {/* filter tabs with counts */}
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        {tabs.map((t) => {
          const active = tab === t.key;
          return (
            <button
              key={t.key}
              onClick={() => setTab(t.key)}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                fontSize: 13,
                fontWeight: 600,
                padding: "7px 14px",
                borderRadius: 99,
                cursor: "pointer",
                border: active ? "1px solid #6366f1" : "1px solid var(--border)",
                background: active ? "rgba(99,102,241,0.10)" : "var(--card)",
                color: active ? "#6366f1" : "var(--foreground)",
              }}
            >
              {t.label}
              <span
                style={{
                  fontSize: 11,
                  fontWeight: 700,
                  padding: "1px 7px",
                  borderRadius: 99,
                  background: active ? "#6366f1" : "var(--muted)",
                  color: active ? "#fff" : "var(--muted-foreground)",
                }}
              >
                {t.count}
              </span>
            </button>
          );
        })}
      </div>

      {/* timeline histogram */}
      <div style={card}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            marginBottom: 14,
          }}
        >
          <div style={{ fontSize: 14, fontWeight: 700, color: "var(--foreground)" }}>
            Timeline
          </div>
          <div
            style={{
              display: "flex",
              gap: 2,
              padding: 2,
              borderRadius: 99,
              background: "var(--muted)",
              border: "1px solid var(--border)",
            }}
          >
            {(["24h", "week"] as const).map((r) => (
              <button
                key={r}
                onClick={() => setRange(r)}
                style={{
                  fontSize: 12,
                  fontWeight: 600,
                  border: "none",
                  borderRadius: 99,
                  padding: "4px 12px",
                  cursor: "pointer",
                  color: range === r ? "#fff" : "var(--muted-foreground)",
                  background: range === r ? "#6366f1" : "transparent",
                }}
              >
                {r === "24h" ? "24hr" : "Past week"}
              </button>
            ))}
          </div>
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "flex-end",
            gap: range === "24h" ? 3 : 10,
            height: 120,
          }}
        >
          {(data?.timeline ?? []).map((b, i) => (
            <div
              key={i}
              style={{
                flex: 1,
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                gap: 6,
                minWidth: 0,
              }}
              title={`${b.label}: ${b.count}`}
            >
              <div
                style={{
                  width: "100%",
                  maxWidth: range === "24h" ? 16 : 40,
                  height: `${Math.max(2, (b.count / maxBar) * 90)}px`,
                  background:
                    b.count > 0 ? "#6366f1" : "var(--border)",
                  borderRadius: 4,
                  transition: "height 0.3s",
                }}
              />
              <div
                style={{
                  fontSize: 9.5,
                  color: "var(--muted-foreground)",
                  whiteSpace: "nowrap",
                }}
              >
                {range === "24h" && i % 3 !== 0 ? "" : b.label}
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* task table */}
      <div style={{ ...card, padding: 0, overflow: "hidden" }}>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "110px 1fr 90px 90px 120px",
            gap: 12,
            padding: "12px 18px",
            borderBottom: "1px solid var(--border)",
            fontSize: 11,
            fontWeight: 700,
            letterSpacing: 0.5,
            color: "var(--muted-foreground)",
            textTransform: "uppercase",
          }}
        >
          <div>Time</div>
          <div>Task</div>
          <div style={{ textAlign: "right" }}>Cost</div>
          <div style={{ textAlign: "right" }}>Duration</div>
          <div>Status</div>
        </div>

        {loading && !data ? (
          <div
            style={{
              padding: 40,
              textAlign: "center",
              fontSize: 13,
              color: "var(--muted-foreground)",
            }}
          >
            Loading…
          </div>
        ) : filtered.length === 0 ? (
          <div
            style={{
              padding: 40,
              textAlign: "center",
              fontSize: 13,
              color: "var(--muted-foreground)",
            }}
          >
            No tasks {tab !== "all" ? `in “${tabs.find((t) => t.key === tab)?.label}”` : "yet"}.
          </div>
        ) : (
          filtered.map((t) => {
            const ss = STATUS_STYLE[t.status];
            return (
              <div
                key={t.jobId}
                style={{
                  display: "grid",
                  gridTemplateColumns: "110px 1fr 90px 90px 120px",
                  gap: 12,
                  padding: "13px 18px",
                  borderBottom: "1px solid var(--border)",
                  fontSize: 13,
                  alignItems: "center",
                }}
              >
                <div style={{ color: "var(--muted-foreground)", fontSize: 12 }}>
                  {fmtTime(t.time)}
                </div>
                <div style={{ minWidth: 0 }}>
                  <div
                    style={{
                      color: "var(--foreground)",
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                    }}
                  >
                    {t.prompt || "—"}
                  </div>
                  <div
                    style={{
                      fontSize: 11.5,
                      color: "var(--muted-foreground)",
                      marginTop: 2,
                    }}
                  >
                    {t.agentEmoji} {t.agentName}
                  </div>
                </div>
                <div
                  style={{
                    textAlign: "right",
                    fontVariantNumeric: "tabular-nums",
                    color: "var(--foreground)",
                  }}
                >
                  {fmtCost(t.cost)}
                </div>
                <div
                  style={{
                    textAlign: "right",
                    fontVariantNumeric: "tabular-nums",
                    color: "var(--muted-foreground)",
                  }}
                >
                  {fmtDuration(t.durationMs)}
                </div>
                <div>
                  <span
                    style={{
                      display: "inline-flex",
                      alignItems: "center",
                      gap: 6,
                      fontSize: 12,
                      fontWeight: 600,
                      padding: "3px 10px",
                      borderRadius: 99,
                      background: ss.bg,
                      color: ss.fg,
                    }}
                  >
                    <span
                      style={{
                        width: 6,
                        height: 6,
                        borderRadius: 99,
                        background: ss.dot,
                      }}
                    />
                    {t.status}
                  </span>
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
