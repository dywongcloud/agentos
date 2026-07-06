"use client";

// app/ui/agents/NewWorkforceModal.tsx
//
// "New workforce" builder for the /ui/agents canvas. The user writes a
// free-form team description (agents + stages) and picks the trigger:
//   auto      — let the compiler infer the trigger from the description
//   schedule  — cron + timezone (with quick presets)
//   app event — searchable catalog of Composio trigger types MERGED with our
//               custom polling types (monday.com etc.); custom ones surface
//               their config fields (e.g. board_id)
//   webhook   — URL is minted at creation and shown in the result
//   chat      — regex pattern on inbound chat messages
// Submits POST /api/ui/agents {op:"create_workforce"} and reloads on done.

import { useCallback, useEffect, useState } from "react";
import type { CSSProperties } from "react";

type TriggerKindChoice = "auto" | "schedule" | "composio" | "webhook" | "chat";

type CatalogTrigger = {
  slug: string;
  name: string;
  description: string;
  toolkit: string | null;
  kind: "composio" | "custom_polling";
  configSchema: {
    properties?: Record<string, { type?: string; description?: string }>;
    required?: string[];
  } | null;
};

type CreateResult = {
  ok: boolean;
  teamId: string;
  name: string;
  emoji: string | null;
  triggerLabel: string;
  note: string;
  stages: number;
  members: Array<{ id: string; name: string; emoji: string }>;
  newAgents: Array<{ id: string; name: string }>;
};

const SCHEDULE_PRESETS: Array<{ label: string; cron: string }> = [
  { label: "Daily 8am", cron: "0 8 * * *" },
  { label: "Weekdays 9am", cron: "0 9 * * 1-5" },
  { label: "Hourly", cron: "0 * * * *" },
  { label: "Mondays 9am", cron: "0 9 * * 1" },
];

export default function NewWorkforceModal({
  userId,
  onClose,
}: {
  userId: string;
  onClose: () => void;
}) {
  const [description, setDescription] = useState("");
  const [kind, setKind] = useState<TriggerKindChoice>("auto");

  // schedule
  const [cron, setCron] = useState("0 8 * * *");
  const [tz, setTz] = useState("America/New_York");

  // chat
  const [pattern, setPattern] = useState("");

  // composio / custom catalog
  const [toolkit, setToolkit] = useState("");
  const [keyword, setKeyword] = useState("");
  const [catalog, setCatalog] = useState<CatalogTrigger[]>([]);
  const [connected, setConnected] = useState<string[]>([]);
  const [loadingCatalog, setLoadingCatalog] = useState(false);
  const [selectedSlug, setSelectedSlug] = useState<string | null>(null);
  const [configValues, setConfigValues] = useState<Record<string, string>>({});

  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CreateResult | null>(null);

  const selected = catalog.find((t) => t.slug === selectedSlug) ?? null;

  const loadCatalog = useCallback(async () => {
    setLoadingCatalog(true);
    setError(null);
    try {
      const params = new URLSearchParams({ op: "triggers", userId });
      if (toolkit.trim()) params.set("toolkit", toolkit.trim().toLowerCase());
      if (keyword.trim()) params.set("keyword", keyword.trim());
      const res = await fetch(`/api/ui/agents?${params}`, { cache: "no-store" });
      if (!res.ok) throw new Error(`catalog fetch failed (${res.status})`);
      const data = (await res.json()) as {
        triggers: CatalogTrigger[];
        connected: string[];
      };
      setCatalog(data.triggers ?? []);
      setConnected(data.connected ?? []);
    } catch (err: any) {
      setError(String(err?.message ?? err));
    } finally {
      setLoadingCatalog(false);
    }
  }, [userId, toolkit, keyword]);

  // Load the catalog the first time the App-event tab is opened.
  useEffect(() => {
    if (kind === "composio" && catalog.length === 0 && !loadingCatalog) {
      void loadCatalog();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind]);

  const requiredKeys = selected?.configSchema?.required ?? [];
  const configKeys = Object.keys(selected?.configSchema?.properties ?? {});

  const canSubmit =
    description.trim().length >= 10 &&
    !submitting &&
    (kind !== "composio" ||
      (selectedSlug !== null &&
        requiredKeys.every((k) => (configValues[k] ?? "").trim() !== ""))) &&
    (kind !== "chat" || pattern.trim() !== "") &&
    (kind !== "schedule" || cron.trim() !== "");

  const submit = useCallback(async () => {
    if (!canSubmit) return;
    setSubmitting(true);
    setError(null);
    try {
      const trigger =
        kind === "auto"
          ? undefined
          : kind === "schedule"
            ? { kind: "schedule", cron: cron.trim(), tz: tz.trim() || undefined }
            : kind === "composio"
              ? { kind: "composio", triggerType: selectedSlug ?? "" }
              : kind === "chat"
                ? { kind: "chat", pattern: pattern.trim() }
                : { kind: "webhook" };
      const triggerConfig =
        kind === "composio" && configKeys.length
          ? Object.fromEntries(
              configKeys
                .map((k) => [k, configValues[k]?.trim() ?? ""])
                .filter(([, v]) => v !== "")
            )
          : undefined;
      const res = await fetch(
        `/api/ui/agents?userId=${encodeURIComponent(userId)}`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            op: "create_workforce",
            description: description.trim(),
            ...(trigger ? { trigger } : {}),
            ...(triggerConfig ? { triggerConfig } : {}),
          }),
        }
      );
      const data = await res.json();
      if (!res.ok) throw new Error(data?.error ?? `create failed (${res.status})`);
      setResult(data as CreateResult);
    } catch (err: any) {
      setError(String(err?.message ?? err));
    } finally {
      setSubmitting(false);
    }
  }, [
    canSubmit,
    kind,
    cron,
    tz,
    selectedSlug,
    pattern,
    configKeys,
    configValues,
    description,
    userId,
  ]);

  return (
    <div style={S.overlay} onClick={onClose}>
      <div style={S.modal} onClick={(e) => e.stopPropagation()}>
        {result ? (
          <>
            <h2 style={S.title}>
              {result.emoji ?? "🤝"} {result.name} created
            </h2>
            <div style={S.label}>Trigger</div>
            <p style={S.body}>{result.triggerLabel}</p>
            {result.note && (
              <>
                <div style={S.label}>Note</div>
                <p style={{ ...S.body, wordBreak: "break-all" }}>{result.note}</p>
              </>
            )}
            <div style={S.label}>Team</div>
            <p style={S.body}>
              {result.stages} stage{result.stages === 1 ? "" : "s"} ·{" "}
              {result.members.map((m) => `${m.emoji} ${m.name}`).join(", ")}
              {result.newAgents.length > 0 && (
                <> · new: {result.newAgents.map((a) => a.name).join(", ")}</>
              )}
            </p>
            <div style={{ display: "flex", gap: 8, marginTop: 18 }}>
              <button style={S.primaryBtn} onClick={() => location.reload()}>
                Open on canvas
              </button>
            </div>
          </>
        ) : (
          <>
            <h2 style={S.title}>New workforce</h2>

            <div style={S.label}>Team description</div>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="e.g. Scout triages my gmail inbox, then a Writer agent drafts replies and a Notifier posts a summary"
              rows={3}
              style={S.textarea}
            />

            <div style={S.label}>Trigger</div>
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
              {(
                [
                  ["auto", "✨ Auto"],
                  ["schedule", "🕐 Schedule"],
                  ["composio", "✉️ App event"],
                  ["webhook", "🔗 Webhook"],
                  ["chat", "💬 Chat"],
                ] as Array<[TriggerKindChoice, string]>
              ).map(([k, label]) => (
                <button
                  key={k}
                  onClick={() => setKind(k)}
                  style={{
                    ...S.segBtn,
                    ...(kind === k ? S.segBtnActive : {}),
                  }}
                >
                  {label}
                </button>
              ))}
            </div>

            {kind === "auto" && (
              <p style={S.hint}>
                The compiler infers the trigger from your description — mention
                it explicitly, e.g. “every weekday at 8am EST” or “when a new
                gmail arrives”.
              </p>
            )}

            {kind === "schedule" && (
              <div style={{ marginTop: 10 }}>
                <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginBottom: 8 }}>
                  {SCHEDULE_PRESETS.map((p) => (
                    <button
                      key={p.cron}
                      onClick={() => setCron(p.cron)}
                      style={{
                        ...S.chipBtn,
                        ...(cron === p.cron ? S.chipBtnActive : {}),
                      }}
                    >
                      {p.label}
                    </button>
                  ))}
                </div>
                <div style={{ display: "flex", gap: 8 }}>
                  <div style={{ flex: 1 }}>
                    <div style={S.fieldLabel}>cron</div>
                    <input
                      value={cron}
                      onChange={(e) => setCron(e.target.value)}
                      placeholder="0 8 * * *"
                      style={S.input}
                    />
                  </div>
                  <div style={{ flex: 1 }}>
                    <div style={S.fieldLabel}>timezone</div>
                    <input
                      value={tz}
                      onChange={(e) => setTz(e.target.value)}
                      placeholder="America/New_York"
                      style={S.input}
                    />
                  </div>
                </div>
              </div>
            )}

            {kind === "composio" && (
              <div style={{ marginTop: 10 }}>
                <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
                  <input
                    value={toolkit}
                    onChange={(e) => setToolkit(e.target.value)}
                    placeholder="toolkit (gmail, monday, github…)"
                    style={{ ...S.input, flex: 1 }}
                    onKeyDown={(e) => e.key === "Enter" && void loadCatalog()}
                  />
                  <input
                    value={keyword}
                    onChange={(e) => setKeyword(e.target.value)}
                    placeholder="keyword"
                    style={{ ...S.input, flex: 1 }}
                    onKeyDown={(e) => e.key === "Enter" && void loadCatalog()}
                  />
                  <button style={S.secondaryBtn} onClick={() => void loadCatalog()}>
                    {loadingCatalog ? "…" : "Search"}
                  </button>
                </div>
                {connected.length > 0 && (
                  <p style={{ ...S.hint, marginTop: 0 }}>
                    connected: {connected.join(", ")}
                  </p>
                )}
                <div style={S.catalog}>
                  {loadingCatalog && <p style={S.hint}>Loading triggers…</p>}
                  {!loadingCatalog && catalog.length === 0 && (
                    <p style={S.hint}>
                      No triggers loaded — pick a toolkit and hit Search.
                    </p>
                  )}
                  {catalog.map((t) => {
                    const active = t.slug === selectedSlug;
                    return (
                      <button
                        key={t.slug}
                        onClick={() => {
                          setSelectedSlug(t.slug);
                          setConfigValues({});
                        }}
                        style={{
                          ...S.catalogRow,
                          ...(active ? S.catalogRowActive : {}),
                        }}
                        title={t.description}
                      >
                        <span style={{ fontWeight: 600, fontSize: 12.5 }}>
                          {t.name}
                        </span>
                        <span style={S.slugText}>{t.slug}</span>
                        <span style={{ flex: 1 }} />
                        {t.toolkit && <span style={S.tkBadge}>{t.toolkit}</span>}
                        {t.kind === "custom_polling" && (
                          <span style={S.pollBadge}>polling</span>
                        )}
                      </button>
                    );
                  })}
                </div>
                {selected && configKeys.length > 0 && (
                  <div style={{ marginTop: 10 }}>
                    {configKeys.map((k) => (
                      <div key={k} style={{ marginBottom: 8 }}>
                        <div style={S.fieldLabel}>
                          {k}
                          {requiredKeys.includes(k) ? " *" : ""}
                          {selected.configSchema?.properties?.[k]?.description && (
                            <span style={{ fontWeight: 400, marginLeft: 6 }}>
                              — {selected.configSchema.properties[k].description}
                            </span>
                          )}
                        </div>
                        <input
                          value={configValues[k] ?? ""}
                          onChange={(e) =>
                            setConfigValues((v) => ({ ...v, [k]: e.target.value }))
                          }
                          style={S.input}
                        />
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {kind === "webhook" && (
              <p style={S.hint}>
                A unique webhook URL is minted at creation — POST anything to it
                to fire the team. The URL appears after you create.
              </p>
            )}

            {kind === "chat" && (
              <div style={{ marginTop: 10 }}>
                <div style={S.fieldLabel}>message pattern (regex)</div>
                <input
                  value={pattern}
                  onChange={(e) => setPattern(e.target.value)}
                  placeholder="e.g. ^standup"
                  style={S.input}
                />
              </div>
            )}

            {error && <p style={S.error}>{error}</p>}

            <div style={{ display: "flex", gap: 8, marginTop: 18 }}>
              <button
                style={{
                  ...S.primaryBtn,
                  opacity: canSubmit ? 1 : 0.5,
                  cursor: canSubmit ? "pointer" : "default",
                }}
                disabled={!canSubmit}
                onClick={() => void submit()}
              >
                {submitting ? "Compiling team…" : "Create workforce"}
              </button>
              <button style={S.secondaryBtn} onClick={onClose}>
                Cancel
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

const S: Record<string, CSSProperties> = {
  overlay: {
    position: "fixed",
    inset: 0,
    background: "rgba(15,17,24,0.45)",
    display: "flex",
    alignItems: "flex-start",
    justifyContent: "center",
    paddingTop: 70,
    zIndex: 50,
  },
  modal: {
    width: 560,
    maxWidth: "calc(100vw - 32px)",
    maxHeight: "calc(100vh - 110px)",
    overflowY: "auto",
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderRadius: 14,
    padding: 22,
    boxShadow: "0 18px 50px rgba(0,0,0,0.25)",
  },
  title: {
    fontSize: 17,
    fontWeight: 600,
    color: "var(--foreground)",
    margin: 0,
  },
  label: {
    fontSize: 10,
    fontWeight: 700,
    letterSpacing: 0.8,
    textTransform: "uppercase",
    color: "var(--muted-foreground)",
    marginTop: 16,
    marginBottom: 6,
  },
  fieldLabel: {
    fontSize: 11,
    fontWeight: 600,
    color: "var(--muted-foreground)",
    marginBottom: 4,
  },
  textarea: {
    width: "100%",
    boxSizing: "border-box",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 9,
    padding: "9px 11px",
    fontSize: 13,
    color: "var(--foreground)",
    resize: "vertical",
    fontFamily: "inherit",
  },
  input: {
    width: "100%",
    boxSizing: "border-box",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 8,
    padding: "7px 10px",
    fontSize: 12.5,
    color: "var(--foreground)",
  },
  segBtn: {
    fontSize: 12.5,
    fontWeight: 600,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 99,
    padding: "6px 13px",
    cursor: "pointer",
  },
  segBtnActive: {
    color: "#fff",
    background: "#6366f1",
    border: "1px solid #6366f1",
  },
  chipBtn: {
    fontSize: 11.5,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 99,
    padding: "4px 10px",
    cursor: "pointer",
  },
  chipBtnActive: {
    color: "#6366f1",
    border: "1px solid #6366f1",
  },
  hint: {
    fontSize: 12,
    color: "var(--muted-foreground)",
    lineHeight: 1.5,
    marginTop: 8,
    marginBottom: 0,
  },
  catalog: {
    border: "1px solid var(--border)",
    borderRadius: 10,
    maxHeight: 210,
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
  },
  catalogRow: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    padding: "8px 11px",
    background: "transparent",
    border: "none",
    borderBottom: "1px solid var(--border)",
    cursor: "pointer",
    textAlign: "left",
    color: "var(--foreground)",
  },
  catalogRowActive: {
    background: "rgba(99,102,241,0.10)",
    outline: "1.5px solid #6366f1",
    outlineOffset: -1.5,
    borderRadius: 8,
  },
  slugText: {
    fontSize: 10.5,
    color: "var(--muted-foreground)",
    fontFamily: "ui-monospace, monospace",
  },
  tkBadge: {
    fontSize: 10,
    fontWeight: 600,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 99,
    padding: "1px 8px",
  },
  pollBadge: {
    fontSize: 10,
    fontWeight: 700,
    color: "#b45309",
    background: "#fef3c7",
    border: "1px solid #fde68a",
    borderRadius: 99,
    padding: "1px 8px",
  },
  error: {
    fontSize: 12.5,
    color: "var(--destructive)",
    marginTop: 10,
    marginBottom: 0,
  },
  primaryBtn: {
    fontSize: 13,
    fontWeight: 600,
    color: "#fff",
    background: "#6366f1",
    border: "none",
    borderRadius: 9,
    padding: "9px 18px",
    cursor: "pointer",
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
