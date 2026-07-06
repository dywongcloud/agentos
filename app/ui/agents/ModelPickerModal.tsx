"use client";

// app/ui/agents/ModelPickerModal.tsx
//
// "Select your Agent's AI Model" picker (modeled on Relevance AI's modal). A
// searchable left-hand list of every catalog model — each row shows the vendor,
// context window and credit-consumption tier — and a right-hand detail panel
// with the description, capability chips and a "Select model" button. Choosing
// a model POSTs {op:"set_agent_model"} to /api/ui/agents; clearing it back to
// the workspace default POSTs modelName:null.
//
// Fetches GET ?op=models for the catalog + the current default model id.

import { useCallback, useEffect, useMemo, useState } from "react";
import type { CSSProperties } from "react";

type CreditTier = "low" | "moderate" | "high";

type ModelInfo = {
  id: string;
  label: string;
  vendor: string;
  description: string;
  contextWindow: number;
  outputLimit: number;
  creditTier: CreditTier;
  reasoning: boolean;
  recommended?: boolean;
};

const TIER_LABEL: Record<CreditTier, string> = {
  low: "Low",
  moderate: "Moderate",
  high: "High",
};

function tierDots(tier: CreditTier): number {
  return tier === "low" ? 1 : tier === "moderate" ? 2 : 3;
}

function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
  return String(n);
}

export default function ModelPickerModal({
  userId,
  agentId,
  agentName,
  current,
  onClose,
  onSaved,
}: {
  userId: string;
  agentId: string;
  agentName: string;
  current: string | null;
  onClose: () => void;
  onSaved: (modelName: string | null) => void;
}) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [defaultModel, setDefaultModel] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [focused, setFocused] = useState<string | null>(current);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const res = await fetch(
          `/api/ui/agents?op=models&userId=${encodeURIComponent(userId)}`,
          { cache: "no-store" }
        );
        if (!res.ok) throw new Error(`models fetch failed (${res.status})`);
        const data = (await res.json()) as {
          models: ModelInfo[];
          defaultModel: string;
        };
        if (!alive) return;
        setModels(data.models ?? []);
        setDefaultModel(data.defaultModel ?? null);
        setFocused((f) => f ?? data.models?.[0]?.id ?? null);
      } catch (err: any) {
        if (alive) setError(String(err?.message ?? err));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [userId]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return models;
    return models.filter(
      (m) =>
        m.label.toLowerCase().includes(q) ||
        m.vendor.toLowerCase().includes(q) ||
        m.id.toLowerCase().includes(q) ||
        m.description.toLowerCase().includes(q)
    );
  }, [models, query]);

  const focusedModel = models.find((m) => m.id === focused) ?? null;

  const save = useCallback(
    async (modelName: string | null) => {
      setSaving(true);
      setError(null);
      try {
        const res = await fetch(
          `/api/ui/agents?userId=${encodeURIComponent(userId)}`,
          {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              op: "set_agent_model",
              agentId,
              modelName,
            }),
          }
        );
        const data = await res.json();
        if (!res.ok) throw new Error(data?.error ?? `save failed (${res.status})`);
        onSaved((data?.modelName ?? null) as string | null);
      } catch (err: any) {
        setError(String(err?.message ?? err));
        setSaving(false);
      }
    },
    [userId, agentId, onSaved]
  );

  return (
    <div style={S.overlay} onClick={onClose}>
      <div style={S.modal} onClick={(e) => e.stopPropagation()}>
        <div style={S.header}>
          <div>
            <h2 style={S.title}>Select {agentName}&apos;s AI model</h2>
            <p style={S.subtitle}>
              The model powers this agent&apos;s reasoning, tool use and replies.
            </p>
          </div>
          <button style={S.closeBtn} onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>

        <div style={S.split}>
          {/* left: searchable list */}
          <div style={S.listCol}>
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search models…"
              style={S.search}
              autoFocus
            />
            <div style={S.list}>
              {loading && <p style={S.hint}>Loading models…</p>}
              {!loading && filtered.length === 0 && (
                <p style={S.hint}>No models match “{query}”.</p>
              )}
              {filtered.map((m) => {
                const active = m.id === focused;
                const isCurrent = m.id === current;
                return (
                  <button
                    key={m.id}
                    onClick={() => setFocused(m.id)}
                    style={{ ...S.row, ...(active ? S.rowActive : {}) }}
                  >
                    <div style={{ minWidth: 0, flex: 1 }}>
                      <div style={S.rowTop}>
                        <span style={S.rowName}>{m.label}</span>
                        {m.recommended && <span style={S.recBadge}>Recommended</span>}
                        {isCurrent && <span style={S.curBadge}>Current</span>}
                      </div>
                      <div style={S.rowMeta}>
                        <span>{m.vendor}</span>
                        <span style={S.dot}>·</span>
                        <span>{fmtTokens(m.contextWindow)} ctx</span>
                        <span style={S.dot}>·</span>
                        <span style={S.creditWrap}>
                          credits
                          <span style={S.dots} aria-hidden>
                            {[0, 1, 2].map((i) => (
                              <span
                                key={i}
                                style={{
                                  ...S.creditDot,
                                  background:
                                    i < tierDots(m.creditTier)
                                      ? CREDIT_COLOR
                                      : "var(--border)",
                                }}
                              />
                            ))}
                          </span>
                        </span>
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          </div>

          {/* right: detail panel */}
          <div style={S.detailCol}>
            {focusedModel ? (
              <>
                <div style={S.detailHead}>
                  <div style={S.detailName}>{focusedModel.label}</div>
                  <div style={S.detailVendor}>{focusedModel.vendor}</div>
                </div>
                <p style={S.detailDesc}>{focusedModel.description}</p>

                <div style={S.specGrid}>
                  <Spec label="Context window" value={`${fmtTokens(focusedModel.contextWindow)} tokens`} />
                  <Spec label="Max output" value={`${fmtTokens(focusedModel.outputLimit)} tokens`} />
                  <Spec label="Credit use" value={TIER_LABEL[focusedModel.creditTier]} />
                  <Spec
                    label="Reasoning"
                    value={focusedModel.reasoning ? "Adaptive thinking" : "Standard"}
                  />
                </div>

                <div style={S.capRow}>
                  <span style={S.cap}>🛠 Tool use</span>
                  {focusedModel.reasoning && <span style={S.cap}>🧠 Reasoning</span>}
                  <span style={S.cap}>📝 Structured output</span>
                </div>

                {focusedModel.id === defaultModel && (
                  <p style={S.note}>This is the current workspace default model.</p>
                )}

                {error && <p style={S.error}>{error}</p>}

                <div style={S.actions}>
                  <button
                    style={{
                      ...S.primaryBtn,
                      opacity: saving || focusedModel.id === current ? 0.55 : 1,
                      cursor:
                        saving || focusedModel.id === current ? "default" : "pointer",
                    }}
                    disabled={saving || focusedModel.id === current}
                    onClick={() => void save(focusedModel.id)}
                  >
                    {focusedModel.id === current
                      ? "Currently selected"
                      : saving
                        ? "Saving…"
                        : "Select model"}
                  </button>
                  {current != null && (
                    <button
                      style={S.secondaryBtn}
                      disabled={saving}
                      onClick={() => void save(null)}
                    >
                      Use workspace default
                    </button>
                  )}
                </div>
              </>
            ) : (
              <p style={S.hint}>Select a model to see its details.</p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function Spec({ label, value }: { label: string; value: string }) {
  return (
    <div style={S.spec}>
      <div style={S.specLabel}>{label}</div>
      <div style={S.specValue}>{value}</div>
    </div>
  );
}

const ACCENT = "#6366f1";
const CREDIT_COLOR = "#6366f1";

const S: Record<string, CSSProperties> = {
  overlay: {
    position: "fixed",
    inset: 0,
    background: "rgba(15,17,24,0.45)",
    display: "flex",
    alignItems: "flex-start",
    justifyContent: "center",
    paddingTop: 60,
    zIndex: 60,
  },
  modal: {
    width: 760,
    maxWidth: "calc(100vw - 32px)",
    maxHeight: "calc(100vh - 100px)",
    display: "flex",
    flexDirection: "column",
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 14,
    boxShadow: "0 18px 50px rgba(0,0,0,0.25)",
    overflow: "hidden",
  },
  header: {
    display: "flex",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 12,
    padding: "20px 22px 14px",
    borderBottom: "1px solid var(--border)",
  },
  title: { fontSize: 17, fontWeight: 600, color: "var(--foreground)", margin: 0 },
  subtitle: {
    fontSize: 12.5,
    color: "var(--muted-foreground)",
    margin: "4px 0 0",
  },
  closeBtn: {
    fontSize: 14,
    color: "var(--muted-foreground)",
    background: "transparent",
    border: "none",
    cursor: "pointer",
    lineHeight: 1,
    padding: 4,
  },
  split: { display: "flex", minHeight: 0, flex: 1 },
  listCol: {
    width: 360,
    flexShrink: 0,
    borderRight: "1px solid var(--border)",
    display: "flex",
    flexDirection: "column",
    minHeight: 0,
  },
  search: {
    margin: 12,
    boxSizing: "border-box",
    width: "calc(100% - 24px)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    padding: "8px 11px",
    fontSize: 13,
    color: "var(--foreground)",
  },
  list: { overflowY: "auto", padding: "0 8px 12px", display: "flex", flexDirection: "column", gap: 4 },
  hint: { fontSize: 12.5, color: "var(--muted-foreground)", padding: "8px 6px", margin: 0 },
  row: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    padding: "9px 11px",
    background: "transparent",
    border: "1px solid transparent",
    borderRadius: 9,
    cursor: "pointer",
    textAlign: "left",
    color: "var(--foreground)",
  },
  rowActive: {
    background: "rgba(99,102,241,0.10)",
    border: `1px solid ${ACCENT}`,
  },
  rowTop: { display: "flex", alignItems: "center", gap: 6 },
  rowName: {
    fontSize: 13.5,
    fontWeight: 600,
    color: "var(--foreground)",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  recBadge: {
    fontSize: 9.5,
    fontWeight: 700,
    color: ACCENT,
    background: "rgba(99,102,241,0.12)",
    borderRadius: 99,
    padding: "1px 7px",
    whiteSpace: "nowrap",
  },
  curBadge: {
    fontSize: 9.5,
    fontWeight: 700,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 99,
    padding: "1px 7px",
    whiteSpace: "nowrap",
  },
  rowMeta: {
    display: "flex",
    alignItems: "center",
    gap: 5,
    fontSize: 11,
    color: "var(--muted-foreground)",
    marginTop: 3,
  },
  dot: { opacity: 0.5 },
  creditWrap: { display: "inline-flex", alignItems: "center", gap: 4 },
  dots: { display: "inline-flex", gap: 2, marginLeft: 1 },
  creditDot: { width: 5, height: 5, borderRadius: 99, display: "inline-block" },
  detailCol: {
    flex: 1,
    minWidth: 0,
    padding: 20,
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
  },
  detailHead: { display: "flex", alignItems: "baseline", gap: 8 },
  detailName: { fontSize: 18, fontWeight: 700, color: "var(--foreground)" },
  detailVendor: { fontSize: 12.5, color: "var(--muted-foreground)" },
  detailDesc: {
    fontSize: 13,
    color: "var(--muted-foreground)",
    lineHeight: 1.55,
    margin: "10px 0 0",
  },
  specGrid: {
    display: "grid",
    gridTemplateColumns: "1fr 1fr",
    gap: 10,
    marginTop: 16,
  },
  spec: {
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    padding: "9px 11px",
  },
  specLabel: {
    fontSize: 10,
    fontWeight: 700,
    letterSpacing: 0.6,
    textTransform: "uppercase",
    color: "var(--muted-foreground)",
  },
  specValue: { fontSize: 13.5, fontWeight: 600, color: "var(--foreground)", marginTop: 3 },
  capRow: { display: "flex", flexWrap: "wrap", gap: 6, marginTop: 14 },
  cap: {
    fontSize: 11.5,
    color: "var(--foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 99,
    padding: "3px 10px",
  },
  note: { fontSize: 11.5, color: "var(--muted-foreground)", marginTop: 12, marginBottom: 0 },
  error: { fontSize: 12.5, color: "var(--destructive)", marginTop: 12, marginBottom: 0 },
  actions: { display: "flex", gap: 8, marginTop: "auto", paddingTop: 18 },
  primaryBtn: {
    fontSize: 13,
    fontWeight: 600,
    color: "#fff",
    background: ACCENT,
    border: "none",
    borderRadius: 9,
    padding: "9px 18px",
  },
  secondaryBtn: {
    fontSize: 13,
    fontWeight: 600,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    padding: "9px 16px",
    cursor: "pointer",
  },
};
