// app/ui/workflows/FlowDiagram.tsx
//
// Presentation renderer for the NBRI workflow models in nbriFlows.ts.
// Pure server component, inline styles (matches the rest of /ui/workflows).
// No client JS — it draws a top-to-bottom flow with typed node cards,
// connectors, parallel boxes, and decision branch columns.

import type { AutoLevel, Flow, FlowNode } from "./nbriFlows";

const C = {
  card: "var(--card)",
  border: "var(--border)",
  text: "var(--foreground)",
  muted: "var(--muted-foreground)",
  trigger: "var(--status-running)", // blue
  hook: "#8b5cf6", // purple
  sleep: "var(--status-cancelled)", // amber
  parallel: "var(--status-completed)", // green
  decision: "#f97316", // orange
  loop: "#ef4444", // red
  end: "var(--status-completed)", // green
  step: "var(--border)",
};

const AUTO_COLOR: Record<AutoLevel, string> = {
  auto: "var(--status-completed)",
  draft: "var(--status-running)",
  human: "var(--status-cancelled)",
  external: "var(--muted-foreground)",
};
const AUTO_LABEL: Record<AutoLevel, string> = {
  auto: "AUTO",
  draft: "DRAFT",
  human: "HUMAN",
  external: "EXTERNAL",
};

function typeColor(kind: FlowNode["kind"]): string {
  return (C as Record<string, string>)[kind] ?? C.step;
}

function Connector() {
  return (
    <div
      style={{
        width: 2,
        height: 16,
        background: C.border,
        margin: "0 auto",
      }}
      aria-hidden
    />
  );
}

function autoChip(auto?: AutoLevel) {
  if (!auto) return null;
  return (
    <span
      style={{
        fontSize: 9,
        fontWeight: 700,
        letterSpacing: 0.4,
        color: AUTO_COLOR[auto],
        border: `1px solid ${AUTO_COLOR[auto]}`,
        borderRadius: 5,
        padding: "0 5px",
        whiteSpace: "nowrap",
      }}
    >
      {AUTO_LABEL[auto]}
    </span>
  );
}

function kindBadge(label: string, color: string) {
  return (
    <span
      style={{
        fontSize: 9,
        fontWeight: 700,
        letterSpacing: 0.5,
        textTransform: "uppercase",
        color,
      }}
    >
      {label}
    </span>
  );
}

// A single non-container node card (trigger / hook / step / sleep / loop / end).
function NodeCard({ node }: { node: Exclude<FlowNode, { kind: "parallel" } | { kind: "decision" }> }) {
  const color = typeColor(node.kind);
  const actor =
    "actor" in node && node.actor ? node.actor : undefined;
  const detail = "detail" in node && node.detail ? node.detail : undefined;
  const auto = "auto" in node ? node.auto : undefined;

  return (
    <div
      style={{
        borderLeft: `3px solid ${color}`,
        background: C.card,
        border: `1px solid ${C.border}`,
        borderLeftWidth: 3,
        borderLeftColor: color,
        borderRadius: 8,
        padding: "8px 12px",
        maxWidth: 460,
        margin: "0 auto",
        width: "100%",
        boxSizing: "border-box",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          justifyContent: "space-between",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
          {kindBadge(node.kind, color)}
          {actor ? (
            <span style={{ fontSize: 10, color: C.muted }}>{actor}</span>
          ) : null}
        </div>
        {autoChip(auto)}
      </div>
      <div
        style={{
          fontSize: 13,
          color: C.text,
          fontWeight: node.kind === "trigger" || node.kind === "end" ? 600 : 500,
          marginTop: 3,
          lineHeight: 1.35,
        }}
      >
        {node.label}
      </div>
      {detail ? (
        <div style={{ fontSize: 11, color: C.muted, marginTop: 2 }}>{detail}</div>
      ) : null}
    </div>
  );
}

function ParallelBox({ node }: { node: Extract<FlowNode, { kind: "parallel" }> }) {
  return (
    <div
      style={{
        border: `1.5px dashed ${C.parallel}`,
        borderRadius: 10,
        padding: 10,
        maxWidth: 620,
        margin: "0 auto",
        width: "100%",
        boxSizing: "border-box",
        background: "color-mix(in srgb, var(--status-completed) 6%, transparent)",
      }}
    >
      <div
        style={{
          fontSize: 9,
          fontWeight: 700,
          letterSpacing: 0.5,
          textTransform: "uppercase",
          color: C.parallel,
          marginBottom: 8,
        }}
      >
        ⇉ parallel{node.label ? ` · ${node.label}` : ""}
      </div>
      <div
        style={{
          display: "flex",
          gap: 10,
          flexWrap: "wrap",
          justifyContent: "center",
        }}
      >
        {node.lanes.map((lane, i) => (
          <div
            key={i}
            style={{
              flex: "1 1 160px",
              minWidth: 150,
              border: `1px solid ${C.border}`,
              borderRadius: 8,
              background: C.card,
              padding: "8px 10px",
            }}
          >
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                gap: 6,
              }}
            >
              {lane.actor ? (
                <span style={{ fontSize: 10, color: C.muted }}>{lane.actor}</span>
              ) : (
                <span />
              )}
              {autoChip(lane.auto)}
            </div>
            <div style={{ fontSize: 12, color: C.text, fontWeight: 500, marginTop: 2 }}>
              {lane.label}
            </div>
            {lane.detail ? (
              <div style={{ fontSize: 10, color: C.muted, marginTop: 2 }}>
                {lane.detail}
              </div>
            ) : null}
          </div>
        ))}
      </div>
    </div>
  );
}

function DecisionBox({ node }: { node: Extract<FlowNode, { kind: "decision" }> }) {
  return (
    <div style={{ maxWidth: 760, margin: "0 auto", width: "100%" }}>
      <div
        style={{
          border: `1.5px solid ${C.decision}`,
          borderRadius: 10,
          padding: "8px 14px",
          background: "color-mix(in srgb, #f97316 8%, transparent)",
          textAlign: "center",
          maxWidth: 440,
          margin: "0 auto",
          boxSizing: "border-box",
        }}
      >
        {kindBadge("decision", C.decision)}
        <div style={{ fontSize: 13, fontWeight: 600, color: C.text, marginTop: 2 }}>
          ◇ {node.label}
        </div>
      </div>
      <div
        style={{
          display: "flex",
          gap: 12,
          marginTop: 12,
          flexWrap: "wrap",
          justifyContent: "center",
          alignItems: "flex-start",
        }}
      >
        {node.branches.map((br, i) => (
          <div
            key={i}
            style={{
              flex: "1 1 200px",
              minWidth: 190,
              maxWidth: 320,
              border: `1px solid ${C.border}`,
              borderRadius: 10,
              padding: 10,
              background: "color-mix(in srgb, var(--muted-foreground) 4%, transparent)",
            }}
          >
            <div
              style={{
                fontSize: 11,
                fontWeight: 700,
                color: C.decision,
                textAlign: "center",
                marginBottom: 8,
                padding: "2px 8px",
                border: `1px solid ${C.decision}`,
                borderRadius: 12,
                display: "inline-block",
                width: "100%",
                boxSizing: "border-box",
              }}
            >
              {br.label}
            </div>
            <NodeSequence nodes={br.nodes} />
          </div>
        ))}
      </div>
    </div>
  );
}

// Renders a vertical sequence of nodes with connectors between them.
function NodeSequence({ nodes }: { nodes: FlowNode[] }) {
  return (
    <div style={{ display: "flex", flexDirection: "column" }}>
      {nodes.map((node, i) => (
        <div key={i}>
          {i > 0 ? <Connector /> : null}
          {node.kind === "parallel" ? (
            <ParallelBox node={node} />
          ) : node.kind === "decision" ? (
            <DecisionBox node={node} />
          ) : (
            <NodeCard node={node} />
          )}
        </div>
      ))}
    </div>
  );
}

function Legend() {
  const items: { label: string; color: string }[] = [
    { label: "trigger", color: C.trigger },
    { label: "hook (wait for event)", color: C.hook },
    { label: "step", color: C.muted },
    { label: "sleep (wait)", color: C.sleep },
    { label: "parallel", color: C.parallel },
    { label: "decision", color: C.decision },
    { label: "loop", color: C.loop },
    { label: "end", color: C.end },
  ];
  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        gap: 12,
        fontSize: 11,
        color: C.muted,
        marginBottom: 8,
      }}
    >
      {items.map((it) => (
        <span key={it.label} style={{ display: "inline-flex", alignItems: "center", gap: 5 }}>
          <span
            style={{
              width: 10,
              height: 10,
              borderRadius: 3,
              background: it.color,
              display: "inline-block",
            }}
          />
          {it.label}
        </span>
      ))}
      <span style={{ marginLeft: 8 }}>
        chips: AUTO / DRAFT / HUMAN / EXTERNAL = automation level
      </span>
    </div>
  );
}

export function FlowDiagram({ flow }: { flow: Flow }) {
  return (
    <section
      id={`flow-${flow.id}`}
      style={{
        background: C.card,
        border: `1px solid ${C.border}`,
        borderRadius: 10,
        padding: 18,
        scrollMarginTop: 20,
      }}
    >
      <div style={{ display: "flex", alignItems: "baseline", gap: 10, flexWrap: "wrap" }}>
        <span style={{ fontSize: 11, color: C.muted, fontWeight: 600 }}>{flow.phase}</span>
        <h2 style={{ fontSize: 18, fontWeight: 600, color: C.text, margin: 0 }}>
          {flow.title}
        </h2>
      </div>
      <p style={{ fontSize: 12, color: C.muted, marginTop: 4, marginBottom: 14 }}>
        Trigger: {flow.triggerSummary}
      </p>
      <NodeSequence nodes={flow.nodes} />
    </section>
  );
}

export function FlowLegend() {
  return <Legend />;
}
