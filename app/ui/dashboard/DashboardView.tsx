"use client";

// app/ui/dashboard/DashboardView.tsx
//
// The interactive dashboard: an account-objective selector, an AI command bar
// (natural language → a compiled KPI widget), and a responsive grid of those
// widgets. All mutations go through /api/ui/dashboard; the server compiles the
// spec once (LLM) and every refresh re-runs it deterministically.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";

import { shell } from "@/app/ui/shell/styles";
import WidgetCard from "@/app/ui/charts/WidgetCard";
import MemoryPanel from "@/app/ui/dashboard/MemoryPanel";
import type { WidgetSpec, WidgetData } from "@/app/lib/widgetSpec";
import type { AccountObjective, ObjectiveKind } from "@/app/lib/accountObjective";

type ObjectiveKindOption = { kind: ObjectiveKind; label: string };

const API = "/api/ui/dashboard";

export default function DashboardView({
  userId,
  initialObjective,
  initialSuggestions,
  kinds,
  initialWidgets,
  initialData,
}: {
  userId: string;
  initialObjective: AccountObjective | null;
  initialSuggestions: string[];
  kinds: ObjectiveKindOption[];
  initialWidgets: WidgetSpec[];
  initialData: Record<string, WidgetData | null>;
}) {
  const [objective, setObjective] = useState<AccountObjective | null>(initialObjective);
  const [suggestions, setSuggestions] = useState<string[]>(initialSuggestions);
  const [widgets, setWidgets] = useState<WidgetSpec[]>(initialWidgets);
  const [data, setData] = useState<Record<string, WidgetData | null>>(initialData);
  const [busyIds, setBusyIds] = useState<Set<string>>(new Set());

  const [prompt, setPrompt] = useState("");
  const [creating, setCreating] = useState(false);
  const [headline, setHeadline] = useState(initialObjective?.headline ?? "");
  const [error, setError] = useState<string | null>(null);
  const promptRef = useRef<HTMLInputElement>(null);

  const [paused, setPaused] = useState(false);
  const [pauseBusy, setPauseBusy] = useState(false);

  const apiUrl = useMemo(
    () => `${API}?userId=${encodeURIComponent(userId)}`,
    [userId]
  );

  const markBusy = useCallback((id: string, on: boolean) => {
    setBusyIds((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);

  const post = useCallback(
    async (body: Record<string, unknown>) => {
      const res = await fetch(apiUrl, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      const json = await res.json().catch(() => null);
      if (!res.ok || !json?.ok) {
        throw new Error(json?.error ?? `request failed (${res.status})`);
      }
      return json;
    },
    [apiUrl]
  );

  // --- Pause all -----------------------------------------------------------

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await fetch(`${apiUrl}&op=pause`);
        const json = await res.json().catch(() => null);
        if (!cancelled && json?.ok) setPaused(json.paused === true);
      } catch {
        // non-fatal; default to not paused
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [apiUrl]);

  const togglePause = useCallback(async () => {
    if (pauseBusy) return;
    setPauseBusy(true);
    setError(null);
    const next = !paused;
    setPaused(next); // optimistic
    try {
      const json = await post({ op: "set_pause", paused: next });
      setPaused(json.paused === true);
    } catch (e: any) {
      setPaused(!next); // revert
      setError(String(e?.message ?? e));
    } finally {
      setPauseBusy(false);
    }
  }, [paused, pauseBusy, post]);

  // --- Objective -----------------------------------------------------------

  const applyObjective = useCallback(
    async (kind: ObjectiveKind, nextHeadline?: string) => {
      setError(null);
      try {
        const json = await post({
          op: "set_objective",
          kind,
          headline: nextHeadline ?? headline,
        });
        setObjective(json.objective ?? null);
        if (Array.isArray(json.suggestions)) setSuggestions(json.suggestions);
      } catch (e: any) {
        setError(String(e?.message ?? e));
      }
    },
    [post, headline]
  );

  // --- Create / compile ----------------------------------------------------

  const createWidget = useCallback(
    async (text: string) => {
      const clean = text.trim();
      if (clean.length < 3 || creating) return;
      setCreating(true);
      setError(null);
      try {
        const json = await post({ op: "create", prompt: clean });
        const widget = json.widget as WidgetSpec;
        setWidgets((prev) => [...prev, widget]);
        setData((prev) => ({ ...prev, [widget.id]: json.data ?? null }));
        setPrompt("");
      } catch (e: any) {
        setError(String(e?.message ?? e));
      } finally {
        setCreating(false);
        promptRef.current?.focus();
      }
    },
    [post, creating]
  );

  // --- Per-widget actions --------------------------------------------------

  const refreshWidget = useCallback(
    async (id: string) => {
      markBusy(id, true);
      try {
        const res = await fetch(
          `${apiUrl}&op=data&id=${encodeURIComponent(id)}`,
          { method: "GET" }
        );
        const json = await res.json().catch(() => null);
        if (json?.ok) setData((prev) => ({ ...prev, [id]: json.data ?? null }));
      } finally {
        markBusy(id, false);
      }
    },
    [apiUrl, markBusy]
  );

  const recompileWidget = useCallback(
    async (id: string) => {
      markBusy(id, true);
      setError(null);
      try {
        const json = await post({ op: "recompile", id });
        const widget = json.widget as WidgetSpec;
        setWidgets((prev) => prev.map((w) => (w.id === id ? widget : w)));
        setData((prev) => ({ ...prev, [id]: json.data ?? null }));
      } catch (e: any) {
        setError(String(e?.message ?? e));
      } finally {
        markBusy(id, false);
      }
    },
    [post, markBusy]
  );

  const deleteWidget = useCallback(
    async (id: string) => {
      const snapshot = widgets;
      setWidgets((prev) => prev.filter((w) => w.id !== id));
      try {
        await post({ op: "delete", id });
        setData((prev) => {
          const next = { ...prev };
          delete next[id];
          return next;
        });
      } catch (e: any) {
        setError(String(e?.message ?? e));
        setWidgets(snapshot); // restore on failure
      }
    },
    [post, widgets]
  );

  const isEmpty = widgets.length === 0;

  return (
    <div style={S.wrap}>
      {/* Account-wide pause */}
      <div style={{ ...S.pauseBar, ...(paused ? S.pauseBarOn : null) }}>
        <div style={S.pauseLeft}>
          <span style={{ ...S.pauseDot, ...(paused ? S.pauseDotOn : null) }} />
          <div>
            <div style={S.pauseTitle}>
              {paused ? "All automations & workforces paused" : "Account running"}
            </div>
            <div style={S.pauseSub}>
              {paused
                ? "Nothing fires until you resume — nothing was deleted."
                : "Automations, agents and workforces fire normally."}
            </div>
          </div>
        </div>
        <button
          type="button"
          onClick={togglePause}
          disabled={pauseBusy}
          style={{
            ...S.pauseBtn,
            ...(paused ? S.pauseBtnResume : S.pauseBtnPause),
            opacity: pauseBusy ? 0.6 : 1,
          }}
        >
          {pauseBusy ? "…" : paused ? "Resume all" : "Pause all"}
        </button>
      </div>

      {/* Objective bar */}
      <div style={S.objectiveBar}>
        <div style={S.objectiveLeft}>
          <span style={S.objectiveLabel}>Account objective</span>
          <select
            style={S.select}
            value={objective?.kind ?? "custom"}
            onChange={(e) => applyObjective(e.target.value as ObjectiveKind)}
          >
            {kinds.map((k) => (
              <option key={k.kind} value={k.kind}>
                {k.label}
              </option>
            ))}
          </select>
        </div>
        <input
          style={{ ...shell.input, maxWidth: 360 }}
          placeholder="One line of context — e.g. DTC skincare, scaling Meta ads"
          value={headline}
          onChange={(e) => setHeadline(e.target.value)}
          onBlur={() => {
            if ((headline ?? "") !== (objective?.headline ?? "")) {
              applyObjective(objective?.kind ?? "custom", headline);
            }
          }}
        />
      </div>

      {/* Command bar */}
      <div style={S.commandCard}>
        <div style={S.commandRow}>
          <input
            ref={promptRef}
            style={{ ...shell.input, height: 40, fontSize: 13 }}
            placeholder="Describe a metric to track — e.g. “automation success rate this week”"
            value={prompt}
            disabled={creating}
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") createWidget(prompt);
            }}
          />
          <button
            type="button"
            style={{
              ...shell.submitButton,
              height: 40,
              padding: "0 18px",
              opacity: creating ? 0.6 : 1,
            }}
            disabled={creating || prompt.trim().length < 3}
            onClick={() => createWidget(prompt)}
          >
            {creating ? "Compiling…" : "Add widget"}
          </button>
        </div>
        <div style={S.chips}>
          {suggestions.map((s) => (
            <button
              key={s}
              type="button"
              style={S.chip}
              disabled={creating}
              onClick={() => createWidget(s)}
            >
              {s}
            </button>
          ))}
        </div>
        {error ? <div style={S.error}>{error}</div> : null}
      </div>

      {/* Widget grid */}
      {isEmpty ? (
        <div style={S.empty}>
          <div style={S.emptyTitle}>No widgets yet</div>
          <div style={S.emptyText}>
            Pick a suggestion above or describe any metric in the command bar.
            Each widget is compiled once and refreshes on its own.
          </div>
        </div>
      ) : (
        <div style={S.grid}>
          {widgets.map((w) => (
            <WidgetCard
              key={w.id}
              spec={w}
              data={data[w.id] ?? undefined}
              busy={busyIds.has(w.id)}
              onRefresh={() => refreshWidget(w.id)}
              onRecompile={() => recompileWidget(w.id)}
              onDelete={() => deleteWidget(w.id)}
            />
          ))}
        </div>
      )}

      {/* Growing memory — what the agent has learned + how it solved past tasks */}
      <MemoryPanel userId={userId} />
    </div>
  );
}

const S: Record<string, CSSProperties> = {
  wrap: { display: "flex", flexDirection: "column", gap: 16 },
  pauseBar: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    padding: "12px 16px",
    flexWrap: "wrap",
  },
  pauseBarOn: {
    borderColor: "var(--destructive)",
    background: "color-mix(in srgb, var(--destructive) 8%, var(--card))",
  },
  pauseLeft: { display: "flex", alignItems: "center", gap: 12, minWidth: 0 },
  pauseDot: {
    width: 10,
    height: 10,
    borderRadius: 999,
    background: "var(--status-ok, #16a34a)",
    flex: "0 0 auto",
  },
  pauseDotOn: { background: "var(--destructive)" },
  pauseTitle: { fontSize: 13, fontWeight: 700, color: "var(--foreground)" },
  pauseSub: { fontSize: 11, color: "var(--muted-foreground)" },
  pauseBtn: {
    height: 34,
    padding: "0 16px",
    borderRadius: 9,
    fontSize: 12,
    fontWeight: 700,
    cursor: "pointer",
    border: "1px solid var(--border)",
  },
  pauseBtnPause: {
    background: "var(--destructive)",
    color: "var(--background)",
    borderColor: "var(--destructive)",
  },
  pauseBtnResume: {
    background: "var(--primary)",
    color: "var(--background)",
    borderColor: "var(--primary)",
  },
  objectiveBar: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
    flexWrap: "wrap",
  },
  objectiveLeft: { display: "flex", alignItems: "center", gap: 10 },
  objectiveLabel: {
    fontSize: 11,
    fontWeight: 700,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: "var(--muted-foreground)",
  },
  select: {
    height: 33,
    borderRadius: 9,
    border: "1px solid var(--border)",
    background: "var(--card)",
    color: "var(--foreground)",
    fontSize: 12,
    fontWeight: 600,
    padding: "0 9px",
    cursor: "pointer",
  },
  commandCard: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    padding: 16,
    display: "flex",
    flexDirection: "column",
    gap: 12,
  },
  commandRow: { display: "flex", gap: 10, alignItems: "center" },
  chips: { display: "flex", flexWrap: "wrap", gap: 8 },
  chip: {
    fontSize: 11,
    color: "var(--foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 999,
    padding: "5px 11px",
    cursor: "pointer",
  },
  error: { color: "var(--destructive)", fontSize: 11, lineHeight: 1.5 },
  grid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))",
    gap: 16,
  },
  empty: {
    border: "1px dashed var(--border)",
    borderRadius: 14,
    padding: "40px 20px",
    textAlign: "center",
    display: "flex",
    flexDirection: "column",
    gap: 6,
    alignItems: "center",
  },
  emptyTitle: { fontSize: 15, fontWeight: 700, color: "var(--foreground)" },
  emptyText: {
    fontSize: 12,
    color: "var(--muted-foreground)",
    maxWidth: 420,
    lineHeight: 1.5,
  },
};
