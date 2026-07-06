// app/ui/charts/MiniTable.tsx
//
// Compact table for list-shaped widget data. Reuses the shared table tokens.

import type { CSSProperties } from "react";
import { shell } from "@/app/ui/shell/styles";

export default function MiniTable({
  columns,
  rows,
}: {
  columns?: string[];
  rows: string[][];
}) {
  if (!rows.length) return <div style={emptyStyle}>No rows</div>;
  const colCount = columns?.length ?? rows[0]?.length ?? 1;

  return (
    <div style={shell.tableWrap}>
      <table style={shell.table}>
        {columns && columns.length > 0 && (
          <thead>
            <tr>
              {columns.map((c, i) => (
                <th key={i} style={shell.th}>
                  {c}
                </th>
              ))}
            </tr>
          </thead>
        )}
        <tbody>
          {rows.map((r, ri) => (
            <tr key={ri}>
              {Array.from({ length: colCount }).map((_, ci) => (
                <td key={ci} style={shell.td}>
                  {r[ci] ?? ""}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const emptyStyle: CSSProperties = {
  color: "var(--muted-foreground)",
  fontSize: 11,
  padding: "18px 0",
};
