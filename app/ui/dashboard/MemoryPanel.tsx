"use client";

// app/ui/dashboard/MemoryPanel.tsx
//
// Browsable view of what the agent has learned: typed long-term memories
// (preferences, commands, projects…) and procedural "solutions" (how it solved
// past tasks). Lets the user see + prune the growing memory. Read-only loads on
// mount; delete is the only mutation.

import { useCallback, useEffect, useState, type CSSProperties } from "react";

type MemoryEntry = {
  id: string;
  kind: string;
  title: string;
  summary?: string;
  labels: string[];
};
type Solution = {
  id: string;
  task: string;
  outcome: string;
  source: string;
  ts: number;
};
type MemoryResponse = {
  ok: boolean;
  counts: Record<string, number>;
  recent: MemoryEntry[];
  solutions: Solution[];
};

const API = "/api/ui/dashboard";

export default function MemoryPanel({ userId }: { userId: string }) {
  const [loading, setLoading] = useState(true);
  const [counts, setCounts] = useState<Record<string, number>>({});
  const [recent, setRecent] = useState<MemoryEntry[]>([]);
  const [solutions, setSolutions] = useState<Solution[]>([]);
  const [err, setErr] = useState<string | null>(null);

  const base = `${API}?userId=${encodeURIComponent(userId)}`;

  const load = useCallback(async () => {
    try {
      const res = await fetch(`${base}&op=memory`);
      const json = (await res.json()) as MemoryResponse;
      if (!json?.ok) throw new Error("failed to load memory");
      setCounts(json.counts ?? {});
      setRecent(json.recent ?? []);
      setSolutions(json.solutions ?? []);
    } catch (e: any) {
      setErr(String(e?.message ?? e));
    } finally {
      setLoading(false);
    }
  }, [base]);

  useEffect(() => {
    void load();
  }, [load]);

  const remove = useCallback(
    async (id: string, scope: "typed" | "solution") => {
      // optimistic
      if (scope === "solution") setSolutions((p) => p.filter((s) => s.id !== id));
      else setRecent((p) => p.filter((m) => m.id !== id));
      try {
        await fetch(base, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ op: "mem_delete", id, scope }),
        });
      } catch {
        void load(); // resync on failure
      }
    },
    [base, load]
  );

  const totalTyped = Object.values(counts).reduce((a, b) => a + b, 0);
  const kindEntries = Object.entries(counts).filter(([, n]) => n > 0);

  return (
    <div style={S.section}>
      <div style={S.headerRow}>
        <span style={S.h2}>Memory</span>
        <span style={S.sub}>
          {totalTyped} learned · {solutions.length} solutions
        </span>
      </div>

      {loading ? (
        <div style={S.muted}>Loading memory…</div>
      ) : err ? (
        <div style={S.error}>{err}</div>
      ) : (
        <>
          {kindEntries.length > 0 && (
            <div style={S.chips}>
              {kindEntries.map(([k, n]) => (
                <span key={k} style={S.countChip}>
                  {k.replace(/_/g, " ")} · {n}
                </span>
              ))}
            </div>
          )}

          <div style={S.cols}>
            <div style={S.col}>
              <div style={S.colTitle}>Recently learned</div>
              {recent.length === 0 ? (
                <div style={S.muted}>
                  Nothing yet — it learns as you chat and run tasks.
                </div>
              ) : (
                recent.map((m) => (
                  <div key={m.id} style={S.item}>
                    <div style={S.itemMain}>
                      <span style={S.kind}>{m.kind.replace(/_/g, " ")}</span>
                      <span style={S.title}>{m.title}</span>
                      {m.summary ? <div style={S.summary}>{m.summary}</div> : null}
                    </div>
                    <button
                      type="button"
                      style={S.del}
                      title="Forget this"
                      onClick={() => remove(m.id, "typed")}
                    >
                      ×
                    </button>
                  </div>
                ))
              )}
            </div>

            <div style={S.col}>
              <div style={S.colTitle}>Solutions — how it solved past tasks</div>
              {solutions.length === 0 ? (
                <div style={S.muted}>
                  No solutions captured yet — completed jobs &amp; code tasks land
                  here automatically.
                </div>
              ) : (
                solutions.map((s) => (
                  <div key={s.id} style={S.item}>
                    <div style={S.itemMain}>
                      <span style={S.kind}>{s.source}</span>
                      <span style={S.title}>{s.task}</span>
                      <div style={S.summary}>{s.outcome}</div>
                    </div>
                    <button
                      type="button"
                      style={S.del}
                      title="Forget this"
                      onClick={() => remove(s.id, "solution")}
                    >
                      ×
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>
        </>
      )}
    </div>
  );
}

const S: Record<string, CSSProperties> = {
  section: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    padding: 16,
    display: "flex",
    flexDirection: "column",
    gap: 12,
  },
  headerRow: {
    display: "flex",
    alignItems: "baseline",
    justifyContent: "space-between",
    gap: 10,
  },
  h2: { fontSize: 15, fontWeight: 700, color: "var(--foreground)" },
  sub: { fontSize: 11, color: "var(--muted-foreground)" },
  chips: { display: "flex", flexWrap: "wrap", gap: 6 },
  countChip: {
    fontSize: 10,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 999,
    padding: "3px 9px",
    textTransform: "capitalize",
  },
  cols: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))",
    gap: 14,
  },
  col: { display: "flex", flexDirection: "column", gap: 8, minWidth: 0 },
  colTitle: {
    fontSize: 11,
    fontWeight: 700,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: "var(--muted-foreground)",
  },
  item: {
    display: "flex",
    gap: 8,
    alignItems: "flex-start",
    padding: "8px 10px",
    border: "1px solid var(--border)",
    borderRadius: 10,
    background: "var(--background)",
  },
  itemMain: { display: "flex", flexDirection: "column", gap: 2, minWidth: 0, flex: 1 },
  kind: {
    fontSize: 9,
    fontWeight: 700,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: "var(--primary)",
  },
  title: { fontSize: 12, fontWeight: 600, color: "var(--foreground)" },
  summary: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    lineHeight: 1.4,
    overflow: "hidden",
    display: "-webkit-box",
    WebkitLineClamp: 2,
    WebkitBoxOrient: "vertical",
  },
  del: {
    border: "none",
    background: "transparent",
    color: "var(--muted-foreground)",
    fontSize: 16,
    lineHeight: 1,
    cursor: "pointer",
    padding: "0 2px",
  },
  muted: { fontSize: 11, color: "var(--muted-foreground)" },
  error: { fontSize: 11, color: "var(--destructive)" },
};
