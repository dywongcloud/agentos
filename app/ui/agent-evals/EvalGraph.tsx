"use client";

// Eval-graph view: a fleet-wide "Overall eval score — Last N weeks" bar chart
// with a dashed pass-threshold line, plus a per-agent breakdown (latest overall,
// dimension bars, and a "Degradation detected" flag). Polls the agent-evals API
// so promotions from the governed optimizer show up live. Pure inline SVG/CSS —
// no chart deps — so it renders identically standalone and embedded in a tab.

import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
} from "react";

type WeeklyPoint = { label: string; value: number | null };
type Dimension = { name: string; score: number };
type AgentOverview = {
  id: string;
  name: string;
  emoji: string;
  toolkits: string[];
  threshold: number;
  scoreCount: number;
  latest: { overall: number; ts: number; note: string | null; dimensions: Dimension[] } | null;
  weekly: WeeklyPoint[];
  degraded: boolean;
  degradeDelta: number | null;
  allScores?: { overall: number; ts: number; durationMs?: number }[];
};
type Overview = {
  threshold: number;
  fleetWeekly: WeeklyPoint[];
  agents: AgentOverview[];
};

function scoreColor(v: number, threshold: number): string {
  if (v >= threshold) return "var(--status-completed)";
  if (v >= 70) return "var(--status-running)";
  return "var(--destructive)";
}

function BarChart({
  data,
  threshold,
}: {
  data: WeeklyPoint[];
  threshold: number;
}) {
  const W = 720;
  const H = 240;
  const padL = 34;
  const padR = 12;
  const padT = 16;
  const padB = 26;
  const plotW = W - padL - padR;
  const plotH = H - padT - padB;
  const n = Math.max(1, data.length);
  const slot = plotW / n;
  const barW = Math.min(46, slot * 0.62);
  const yFor = (v: number) => padT + plotH - (Math.max(0, Math.min(100, v)) / 100) * plotH;
  const thresholdY = yFor(threshold);

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      style={{ width: "100%", height: "auto", display: "block" }}
      role="img"
      aria-label="Overall eval score by week"
    >
      {[0, 25, 50, 75, 100].map((g) => (
        <g key={g}>
          <line
            x1={padL}
            x2={W - padR}
            y1={yFor(g)}
            y2={yFor(g)}
            stroke="var(--border)"
            strokeWidth={1}
          />
          <text
            x={padL - 6}
            y={yFor(g) + 3}
            textAnchor="end"
            fontSize={9}
            fill="var(--muted-foreground)"
          >
            {g}
          </text>
        </g>
      ))}

      {data.map((d, i) => {
        if (d.value == null) return null;
        const x = padL + i * slot + (slot - barW) / 2;
        const y = yFor(d.value);
        const h = padT + plotH - y;
        return (
          <g key={i}>
            <rect
              x={x}
              y={y}
              width={barW}
              height={Math.max(1, h)}
              rx={3}
              fill={scoreColor(d.value, threshold)}
              opacity={0.92}
            />
            <text
              x={x + barW / 2}
              y={y - 4}
              textAnchor="middle"
              fontSize={9}
              fontWeight={600}
              fill="var(--foreground)"
            >
              {Math.round(d.value)}
            </text>
            <text
              x={x + barW / 2}
              y={H - 8}
              textAnchor="middle"
              fontSize={9}
              fill="var(--muted-foreground)"
            >
              {d.label}
            </text>
          </g>
        );
      })}

      {/* Pass-threshold line */}
      <line
        x1={padL}
        x2={W - padR}
        y1={thresholdY}
        y2={thresholdY}
        stroke="var(--status-completed)"
        strokeWidth={1.5}
        strokeDasharray="5 4"
      />
      <text
        x={W - padR}
        y={thresholdY - 4}
        textAnchor="end"
        fontSize={9}
        fontWeight={600}
        fill="var(--status-completed)"
      >
        pass {threshold}
      </text>
    </svg>
  );
}

// Area sparkline of a metric over its run history. Pure SVG, gradient fill,
// auto-scaled to the series' own min/max so even subtle drift is visible. Built
// to read well with sparse data too (long-running task evals produce few, slow
// points), drawing dots when there are only a handful of samples.
function Sparkline({
  values,
  color,
  width = 260,
  height = 56,
}: {
  values: number[];
  color: string;
  width?: number;
  height?: number;
}) {
  if (values.length < 2) {
    return (
      <div style={{ height, display: "grid", placeItems: "center", fontSize: 11, color: "var(--muted-foreground)" }}>
        {values.length === 1 ? "one data point — trend appears after the next run" : "no history yet"}
      </div>
    );
  }
  const pad = 4;
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const n = values.length;
  const x = (i: number) => pad + (i / (n - 1)) * (width - pad * 2);
  const y = (v: number) => pad + (1 - (v - min) / span) * (height - pad * 2);
  const pts = values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`);
  const line = `M${pts.join(" L")}`;
  const area = `${line} L${x(n - 1).toFixed(1)},${height - pad} L${x(0).toFixed(1)},${height - pad} Z`;
  const gid = `spark-${color.replace(/[^a-z0-9]/gi, "")}-${n}`;
  const showDots = n <= 8;
  return (
    <svg viewBox={`0 0 ${width} ${height}`} style={{ width: "100%", height: "auto", display: "block" }} role="img" aria-label="metric trend">
      <defs>
        <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity={0.28} />
          <stop offset="100%" stopColor={color} stopOpacity={0} />
        </linearGradient>
      </defs>
      <path d={area} fill={`url(#${gid})`} />
      <path d={line} fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
      {showDots &&
        values.map((v, i) => (
          <circle key={i} cx={x(i)} cy={y(v)} r={2.4} fill={color} />
        ))}
    </svg>
  );
}

function fmtDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 90) return `${s < 10 ? s.toFixed(1) : Math.round(s)}s`;
  const m = s / 60;
  if (m < 90) return `${m.toFixed(m < 10 ? 1 : 0)}m`;
  return `${(m / 60).toFixed(1)}h`;
}

// The evolving metric block per eval: headline score, period-over-period trend
// delta (arrow + signed points), an area sparkline of the score history, and —
// for long-running task evals — an average run-duration stat.
function MetricHeader({ a, threshold }: { a: AgentOverview; threshold: number }) {
  // allScores arrives newest-first; chart oldest→newest.
  const hist = (a.allScores ?? []).slice().reverse();
  const overalls = hist.map((p) => p.overall);
  const durations = hist.map((p) => p.durationMs).filter((d): d is number => typeof d === "number");
  const latest = a.latest?.overall ?? overalls[overalls.length - 1] ?? null;
  const prev = overalls.length >= 2 ? overalls[overalls.length - 2] : null;
  const delta = latest != null && prev != null ? Math.round((latest - prev) * 10) / 10 : null;
  const up = delta != null && delta >= 0;
  const trendColor = delta == null ? "var(--muted-foreground)" : up ? "var(--status-completed)" : "var(--destructive)";
  const avgDur = durations.length ? durations.reduce((x, y) => x + y, 0) / durations.length : null;
  const longRunning = avgDur != null && avgDur >= 20000;
  const lineColor = latest != null ? scoreColor(latest, threshold) : "var(--muted-foreground)";

  return (
    <div style={s.metricBlock}>
      <div style={s.metricTopRow}>
        <div>
          <div style={s.metricLabel}>Overall score</div>
          <div style={s.metricValueRow}>
            <span style={s.metricValue}>{latest != null ? Math.round(latest) : "—"}</span>
            {delta != null && (
              <span style={{ ...s.metricDelta, color: trendColor }}>
                {up ? "▲" : "▼"} {Math.abs(delta)} pts
              </span>
            )}
          </div>
        </div>
        <div style={s.metricStats}>
          <div style={s.metricStat}>
            <span style={s.metricStatNum}>{a.scoreCount}</span>
            <span style={s.metricStatLbl}>runs</span>
          </div>
          {avgDur != null && (
            <div style={s.metricStat} title={longRunning ? "long-running task eval" : undefined}>
              <span style={s.metricStatNum}>
                {longRunning ? "⏱ " : ""}
                {fmtDuration(avgDur)}
              </span>
              <span style={s.metricStatLbl}>avg run</span>
            </div>
          )}
        </div>
      </div>
      <Sparkline values={overalls} color={lineColor} />
    </div>
  );
}

function DimensionRow({ d, threshold }: { d: Dimension; threshold: number }) {
  return (
    <div style={s.dimRow}>
      <div style={s.dimName}>{d.name}</div>
      <div style={s.dimTrack}>
        <div
          style={{
            ...s.dimFill,
            width: `${Math.max(0, Math.min(100, d.score))}%`,
            background: scoreColor(d.score, threshold),
          }}
        />
      </div>
      <div style={{ ...s.dimScore, color: scoreColor(d.score, threshold) }}>
        {Math.round(d.score)}
      </div>
    </div>
  );
}

function AgentCard({ a }: { a: AgentOverview }) {
  const t = a.threshold;
  return (
    <div style={s.agentCard}>
      <div style={s.agentHead}>
        <div style={s.agentIdentity}>
          <span style={s.agentEmoji}>{a.emoji}</span>
          <div>
            <div style={s.agentName}>{a.name}</div>
            <div style={s.agentMeta}>
              {a.toolkits.length ? a.toolkits.join(", ") : "no toolkits"} ·{" "}
              {a.scoreCount} eval{a.scoreCount === 1 ? "" : "s"}
            </div>
          </div>
        </div>
        <div style={s.agentRight}>
          {a.latest ? (
            <div style={{ ...s.overallPill, color: scoreColor(a.latest.overall, t), borderColor: scoreColor(a.latest.overall, t) }}>
              {Math.round(a.latest.overall)}
            </div>
          ) : (
            <div style={s.overallPillMuted}>—</div>
          )}
        </div>
      </div>

      {a.degraded && (
        <div style={s.degradeBadge}>
          ⚠ Degradation detected
          {a.degradeDelta != null ? ` (${a.degradeDelta} vs recent baseline)` : ""}
        </div>
      )}

      {a.latest && <MetricHeader a={a} threshold={t} />}

      {a.latest ? (
        <div style={s.dimList}>
          {a.latest.dimensions.map((d, i) => (
            <DimensionRow key={i} d={d} threshold={t} />
          ))}
        </div>
      ) : (
        <div style={s.emptyAgent}>No eval runs yet — this agent will be scored after its next run.</div>
      )}

      {a.latest?.note && <div style={s.note}>“{a.latest.note}”</div>}
    </div>
  );
}

export default function EvalGraph({
  userId,
  embed,
}: {
  userId: string;
  embed?: boolean;
}) {
  const [data, setData] = useState<Overview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const fetchData = useCallback(async () => {
    try {
      const res = await fetch(
        `/api/ui/agent-evals?op=overview&userId=${encodeURIComponent(userId)}`,
        { cache: "no-store" }
      );
      if (!res.ok) {
        setError(`HTTP ${res.status}`);
        return;
      }
      const json = (await res.json()) as Overview;
      setData(json);
      setError(null);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
    } finally {
      setLoading(false);
    }
  }, [userId]);

  useEffect(() => {
    fetchData();
    const id = setInterval(fetchData, 8000);
    return () => clearInterval(id);
  }, [fetchData]);

  const threshold = data?.threshold ?? 90;
  const sortedAgents = useMemo(() => {
    const list = data?.agents ?? [];
    return [...list].sort((a, b) => {
      // Degraded first, then lowest latest overall, then by name.
      if (a.degraded !== b.degraded) return a.degraded ? -1 : 1;
      const av = a.latest?.overall ?? 999;
      const bv = b.latest?.overall ?? 999;
      if (av !== bv) return av - bv;
      return a.name.localeCompare(b.name);
    });
  }, [data]);

  return (
    <div style={{ ...s.root, ...(embed ? s.rootEmbed : {}) }}>
      <div style={s.chartCard}>
        <div style={s.chartHead}>
          <div>
            <div style={s.chartTitle}>Overall eval score</div>
            <div style={s.chartSub}>Last 12 weeks · fleet-wide mean across all agents</div>
          </div>
          {data && (
            <div style={s.legend}>
              <span style={{ ...s.legendDot, background: "var(--status-completed)" }} /> pass
              <span style={{ ...s.legendDot, background: "var(--status-running)" }} /> warn
              <span style={{ ...s.legendDot, background: "var(--destructive)" }} /> fail
            </div>
          )}
        </div>
        {loading && !data ? (
          <div style={s.placeholder}>Loading eval history…</div>
        ) : error ? (
          <div style={s.errorBox}>Couldn’t load evals: {error}</div>
        ) : (
          <BarChart data={data!.fleetWeekly} threshold={threshold} />
        )}
      </div>

      <div style={s.agentsHead}>
        <div style={s.chartTitle}>Per-agent eval results</div>
        <div style={s.chartSub}>
          Governed self-optimization promotes a persona tweak only when it beats the proven
          baseline. Degraded agents are flagged and surface first.
        </div>
      </div>

      {data && sortedAgents.length === 0 && (
        <div style={s.emptyAll}>
          No agents yet. Create one with <code>/agent create</code> in chat — it will be scored
          after its first run.
        </div>
      )}

      <div style={s.agentGrid}>
        {sortedAgents.map((a) => (
          <AgentCard key={a.id} a={a} />
        ))}
      </div>
    </div>
  );
}

const s: Record<string, CSSProperties> = {
  root: {
    fontFamily:
      'Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
    color: "var(--foreground)",
    display: "flex",
    flexDirection: "column",
    gap: 22,
  },
  rootEmbed: {
    padding: 18,
  },
  chartCard: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    boxShadow: "0 2px 6px rgba(0,0,0,0.04)",
    padding: 18,
  },
  chartHead: {
    display: "flex",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 12,
    marginBottom: 14,
    flexWrap: "wrap",
  },
  chartTitle: { fontSize: 17, fontWeight: 700, lineHeight: 1.2 },
  chartSub: { fontSize: 12, color: "var(--muted-foreground)", marginTop: 3 },
  legend: {
    display: "flex",
    alignItems: "center",
    gap: 6,
    fontSize: 11,
    color: "var(--muted-foreground)",
  },
  legendDot: {
    width: 9,
    height: 9,
    borderRadius: 999,
    display: "inline-block",
    marginLeft: 8,
  },
  placeholder: { padding: 40, textAlign: "center", color: "var(--muted-foreground)", fontSize: 13 },
  errorBox: {
    padding: 18,
    color: "var(--destructive)",
    fontSize: 13,
    border: "1px solid var(--destructive)",
    borderRadius: 10,
  },
  agentsHead: { marginTop: 2 },
  agentGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fill, minmax(330px, 1fr))",
    gap: 14,
  },
  agentCard: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    boxShadow: "0 2px 6px rgba(0,0,0,0.04)",
    padding: 16,
    display: "flex",
    flexDirection: "column",
    gap: 10,
  },
  agentHead: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  agentIdentity: { display: "flex", alignItems: "center", gap: 11, minWidth: 0 },
  agentEmoji: { fontSize: 26, lineHeight: 1 },
  agentName: { fontSize: 14, fontWeight: 700, lineHeight: 1.2 },
  agentMeta: { fontSize: 11, color: "var(--muted-foreground)", marginTop: 2 },
  agentRight: { flexShrink: 0 },
  overallPill: {
    minWidth: 42,
    height: 34,
    padding: "0 10px",
    borderRadius: 10,
    border: "2px solid",
    display: "grid",
    placeItems: "center",
    fontSize: 16,
    fontWeight: 800,
  },
  overallPillMuted: {
    minWidth: 42,
    height: 34,
    padding: "0 10px",
    borderRadius: 10,
    border: "2px solid var(--border)",
    color: "var(--muted-foreground)",
    display: "grid",
    placeItems: "center",
    fontSize: 16,
    fontWeight: 800,
  },
  degradeBadge: {
    fontSize: 11,
    fontWeight: 700,
    color: "var(--destructive)",
    background: "color-mix(in srgb, var(--destructive) 12%, transparent)",
    border: "1px solid var(--destructive)",
    borderRadius: 8,
    padding: "5px 9px",
    width: "fit-content",
  },
  metricBlock: {
    border: "1px solid var(--border)",
    borderRadius: 10,
    background: "color-mix(in srgb, var(--muted) 30%, transparent)",
    padding: "10px 12px 4px",
    display: "flex",
    flexDirection: "column",
    gap: 4,
  },
  metricTopRow: { display: "flex", alignItems: "flex-start", justifyContent: "space-between", gap: 10 },
  metricLabel: { fontSize: 10, color: "var(--muted-foreground)", textTransform: "uppercase", letterSpacing: 0.5 },
  metricValueRow: { display: "flex", alignItems: "baseline", gap: 8, marginTop: 2 },
  metricValue: { fontSize: 26, fontWeight: 800, lineHeight: 1 },
  metricDelta: { fontSize: 12, fontWeight: 700 },
  metricStats: { display: "flex", gap: 14 },
  metricStat: { display: "flex", flexDirection: "column", alignItems: "flex-end" },
  metricStatNum: { fontSize: 13, fontWeight: 700 },
  metricStatLbl: { fontSize: 10, color: "var(--muted-foreground)", textTransform: "uppercase", letterSpacing: 0.5 },
  dimList: { display: "flex", flexDirection: "column", gap: 7 },
  dimRow: { display: "grid", gridTemplateColumns: "minmax(90px, 40%) 1fr 30px", alignItems: "center", gap: 9 },
  dimName: { fontSize: 11, color: "var(--foreground)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  dimTrack: { height: 7, borderRadius: 999, background: "var(--muted)", overflow: "hidden" },
  dimFill: { height: "100%", borderRadius: 999 },
  dimScore: { fontSize: 11, fontWeight: 700, textAlign: "right" },
  note: { fontSize: 11, color: "var(--muted-foreground)", fontStyle: "italic", lineHeight: 1.4 },
  emptyAgent: { fontSize: 11, color: "var(--muted-foreground)" },
  emptyAll: { fontSize: 13, color: "var(--muted-foreground)", padding: 18, border: "1px dashed var(--border)", borderRadius: 12 },
};
