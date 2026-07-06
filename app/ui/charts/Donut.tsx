// app/ui/charts/Donut.tsx
//
// Hand-rolled donut chart (inline SVG). Used for a part-of-whole breakdown
// ("tickets by status"). Slice colors cycle through the status tokens.

import type { CSSProperties } from "react";

const SLICE_COLORS = [
  "var(--status-running)",
  "var(--status-completed)",
  "var(--status-failed)",
  "var(--status-pending)",
  "var(--status-cancelled)",
  "var(--primary)",
];

function polar(cx: number, cy: number, r: number, frac: number) {
  const a = 2 * Math.PI * frac - Math.PI / 2;
  return [cx + r * Math.cos(a), cy + r * Math.sin(a)] as const;
}

export default function Donut({
  series,
}: {
  series: { label: string; value: number }[];
}) {
  const data = series.filter((s) => s.value > 0);
  if (!data.length) return <Empty />;

  const total = data.reduce((a, s) => a + s.value, 0);
  const size = 160;
  const cx = size / 2;
  const cy = size / 2;
  const r = 64;
  const inner = 40;

  let acc = 0;
  const arcs = data.map((s, i) => {
    const start = acc / total;
    acc += s.value;
    const end = acc / total;
    const [x1, y1] = polar(cx, cy, r, start);
    const [x2, y2] = polar(cx, cy, r, end);
    const [ix2, iy2] = polar(cx, cy, inner, end);
    const [ix1, iy1] = polar(cx, cy, inner, start);
    const large = end - start > 0.5 ? 1 : 0;
    const d = [
      `M ${x1.toFixed(2)} ${y1.toFixed(2)}`,
      `A ${r} ${r} 0 ${large} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`,
      `L ${ix2.toFixed(2)} ${iy2.toFixed(2)}`,
      `A ${inner} ${inner} 0 ${large} 0 ${ix1.toFixed(2)} ${iy1.toFixed(2)}`,
      "Z",
    ].join(" ");
    return { d, color: SLICE_COLORS[i % SLICE_COLORS.length], label: s.label, value: s.value };
  });

  return (
    <div style={S.row}>
      <svg viewBox={`0 0 ${size} ${size}`} width={150} height={150} role="img">
        {arcs.map((a, i) => (
          <path key={i} d={a.d} fill={a.color} />
        ))}
        <text x={cx} y={cy + 4} fontSize={18} fontWeight={700} textAnchor="middle" fill="var(--foreground)">
          {total.toLocaleString()}
        </text>
      </svg>
      <div style={S.legend}>
        {arcs.map((a, i) => (
          <div key={i} style={S.legendRow}>
            <span style={{ ...S.swatch, background: a.color }} />
            <span style={S.legendLabel}>{a.label}</span>
            <span style={S.legendValue}>{a.value.toLocaleString()}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function Empty() {
  return <div style={S.empty}>No data</div>;
}

const S: Record<string, CSSProperties> = {
  row: { display: "flex", alignItems: "center", gap: 18, flexWrap: "wrap" },
  legend: { display: "flex", flexDirection: "column", gap: 6, minWidth: 120 },
  legendRow: { display: "flex", alignItems: "center", gap: 8, fontSize: 11 },
  swatch: { width: 10, height: 10, borderRadius: 3, flexShrink: 0 },
  legendLabel: { color: "var(--muted-foreground)", flex: 1 },
  legendValue: { color: "var(--foreground)", fontWeight: 600 },
  empty: { color: "var(--muted-foreground)", fontSize: 11, padding: "18px 0" },
};
