// app/ui/charts/StatTile.tsx
//
// Big-number tile for a single scalar KPI. Pure markup, theme-tokened.

import type { CSSProperties } from "react";

export default function StatTile({
  value,
  unit,
  delta,
}: {
  value: number | string;
  unit?: string;
  delta?: number;
}) {
  const display =
    typeof value === "number" ? value.toLocaleString(undefined, { maximumFractionDigits: 2 }) : value;
  const deltaColor =
    delta == null
      ? undefined
      : delta > 0
        ? "var(--status-completed)"
        : delta < 0
          ? "var(--destructive)"
          : "var(--muted-foreground)";

  return (
    <div style={S.wrap}>
      <div style={S.value}>
        {display}
        {unit ? <span style={S.unit}>{unit}</span> : null}
      </div>
      {delta != null && (
        <div style={{ ...S.delta, color: deltaColor }}>
          {delta > 0 ? "▲" : delta < 0 ? "▼" : "—"} {Math.abs(delta)}
        </div>
      )}
    </div>
  );
}

const S: Record<string, CSSProperties> = {
  wrap: {
    display: "flex",
    flexDirection: "column",
    gap: 6,
    minHeight: 92,
    justifyContent: "center",
  },
  value: {
    fontSize: 40,
    fontWeight: 700,
    lineHeight: 1,
    color: "var(--foreground)",
    letterSpacing: "-0.02em",
  },
  unit: {
    fontSize: 16,
    fontWeight: 600,
    color: "var(--muted-foreground)",
    marginLeft: 6,
  },
  delta: {
    fontSize: 12,
    fontWeight: 600,
  },
};
