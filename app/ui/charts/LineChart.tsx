// app/ui/charts/LineChart.tsx
//
// Hand-rolled line/area chart (inline SVG, scales to container width). Used for
// a metric over time ("conversions over the last 14 days").

import type { CSSProperties } from "react";

export default function LineChart({
  series,
}: {
  series: { label: string; value: number }[];
}) {
  if (series.length < 2) return <Empty />;

  const w = 480;
  const h = 160;
  const padX = 8;
  const padY = 14;
  const max = Math.max(...series.map((s) => s.value), 1);
  const stepX = (w - padX * 2) / (series.length - 1);
  const scaleY = (v: number) => h - padY - (v / max) * (h - padY * 2);

  const points = series.map((s, i) => [padX + i * stepX, scaleY(s.value)] as const);
  const line = points.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const area =
    `${padX},${h - padY} ` +
    points.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ") +
    ` ${padX + (series.length - 1) * stepX},${h - padY}`;

  // Show ~6 evenly spaced x labels so the axis doesn't crowd.
  const labelEvery = Math.max(1, Math.ceil(series.length / 6));

  return (
    <svg viewBox={`0 0 ${w} ${h + 16}`} width="100%" role="img" style={{ display: "block" }}>
      <polygon points={area} fill="var(--status-running)" opacity={0.12} />
      <polyline points={line} fill="none" stroke="var(--status-running)" strokeWidth={2} />
      {points.map(([x, y], i) => (
        <circle key={i} cx={x} cy={y} r={2.5} fill="var(--status-running)" />
      ))}
      {series.map((s, i) =>
        i % labelEvery === 0 ? (
          <text
            key={`l-${i}`}
            x={padX + i * stepX}
            y={h + 12}
            fontSize={9}
            textAnchor="middle"
            fill="var(--muted-foreground)"
          >
            {s.label}
          </text>
        ) : null
      )}
    </svg>
  );
}

function Empty() {
  return <div style={emptyStyle}>Not enough data points</div>;
}

const emptyStyle: CSSProperties = {
  color: "var(--muted-foreground)",
  fontSize: 11,
  padding: "18px 0",
};
