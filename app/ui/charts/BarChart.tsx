// app/ui/charts/BarChart.tsx
//
// Hand-rolled horizontal bar chart (inline SVG, scales to container width).
// Used for dimension breakdowns ("jobs by status").

import type { CSSProperties } from "react";

export default function BarChart({
  series,
}: {
  series: { label: string; value: number }[];
}) {
  if (!series.length) return <Empty />;
  const max = Math.max(...series.map((s) => s.value), 1);
  const rowH = 26;
  const gap = 8;
  const labelW = 96;
  const h = series.length * (rowH + gap);
  const barAreaW = 320;
  const w = labelW + barAreaW + 44;

  return (
    <svg viewBox={`0 0 ${w} ${h}`} width="100%" role="img" style={{ display: "block" }}>
      {series.map((s, i) => {
        const y = i * (rowH + gap);
        const bw = Math.max(2, (s.value / max) * barAreaW);
        return (
          <g key={`${s.label}-${i}`}>
            <text x={0} y={y + rowH / 2 + 4} fontSize={11} fill="var(--muted-foreground)">
              {s.label.length > 14 ? s.label.slice(0, 13) + "…" : s.label}
            </text>
            <rect
              x={labelW}
              y={y}
              width={bw}
              height={rowH}
              rx={5}
              fill="var(--status-running)"
            />
            <text
              x={labelW + bw + 6}
              y={y + rowH / 2 + 4}
              fontSize={11}
              fontWeight={600}
              fill="var(--foreground)"
            >
              {s.value.toLocaleString()}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function Empty() {
  return <div style={emptyStyle}>No data</div>;
}

const emptyStyle: CSSProperties = {
  color: "var(--muted-foreground)",
  fontSize: 11,
  padding: "18px 0",
};
