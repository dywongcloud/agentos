"use client";

// app/ui/agents/WorkforceCanvas.tsx
//
// relay.app-style workforce canvas. One tab per team (plus an "All agents"
// roster): the selected team lays out deterministically on a ReactFlow canvas
// — a floating "⚡ Trigger" badge node, then one column per stage. Agents are
// pill cards with generated 8-bit pixel avatars and icon-only toolkit chips;
// route stages render a dashed "AI routing" pill. Edges are smoothstep dotted
// lines (no arrowheads) with a green "200" badge flowing along them. Hovering
// an agent node reveals an inline command bar to prompt that agent live.
// Clicking a node opens the side panel; a footer bar narrates the live run.
// Polls /api/ui/agents every 6s.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, MouseEvent as ReactMouseEvent } from "react";
import {
  ReactFlow,
  Background,
  BackgroundVariant,
  BaseEdge,
  Controls,
  Handle,
  Position,
  applyNodeChanges,
  getSmoothStepPath,
  type Node,
  type Edge,
  type NodeProps,
  type EdgeProps,
  type OnNodesChange,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { toolkitLogo, toolkitInitials } from "@/app/ui/toolkitLogo";
import PixelAvatar from "@/app/ui/agents/PixelAvatar";
import NewWorkforceModal from "@/app/ui/agents/NewWorkforceModal";
import WorkforceOffice from "@/app/ui/agents/WorkforceOffice";
import ActivityDashboard from "@/app/ui/agents/ActivityDashboard";
import AgentShowcase from "@/app/ui/agents/AgentShowcase";
import ModelPickerModal from "@/app/ui/agents/ModelPickerModal";
import { getModelInfo } from "@/app/lib/modelCatalog";
import type { WorkforceStage, WorkforceStageRecord } from "@/app/lib/agents";

export type CanvasAgent = {
  id: string;
  name: string;
  emoji: string;
  persona: string;
  toolkits: string[];
  skills: string[] | null;
  telegramBotId: string | null;
  modelName: string | null;
};

export type CanvasTeam = {
  id: string;
  name: string;
  emoji: string | null;
  spec: string;
  stages: WorkforceStage[];
  automationId: string;
  enabled: boolean;
  triggerKind: "schedule" | "composio" | "webhook" | "chat" | null;
  triggerLabel: string;
  status: string | null;
  // Optional brand logo for the trigger node (e.g. WeChat), and a static
  // "chat logs" sample shown in the side panel — used by the showcase team.
  triggerLogo?: string | null;
  chatLog?: { who: string; text: string; group?: boolean }[] | null;
};

export type CanvasBot = {
  botId: string;
  agentId: string;
  username: string;
};

type RunListItem = {
  id: string;
  status: "running" | "ok" | "error";
  source: string;
  startedAt: number;
  finishedAt: number | null;
  resultText: string | null;
};

type RunDetail = {
  run: RunListItem;
  stages: WorkforceStageRecord[];
};

// --- layout constants -------------------------------------------------------

const COL_W = 290;
const ACCENT = "var(--status-running)";

// --- node data shapes ---------------------------------------------------------

type TriggerNodeData = {
  label: string;
  kind: string;
  active: boolean;
  logo?: string | null;
};

type AgentNodeData = {
  agent: CanvasAgent | null;
  agentId: string;
  botUsername: string | null;
  stageIndex: number;
  live: boolean;
  outcome: "ok" | "error" | null;
  userId: string;
};

type RouteNodeData = {
  instruction: string;
  candidates: string[];
  picked: string[];
  stageIndex: number;
  live: boolean;
};

// --- shared bits ---------------------------------------------------------------

const pillCard: CSSProperties = {
  background: "var(--card)",
  border: "1px solid var(--border)",
  borderRadius: 16,
  padding: "10px 14px",
  fontFamily:
    "ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif",
  boxShadow: "0 1px 2px rgba(0,0,0,0.06)",
};

const handleDot: CSSProperties = {
  width: 6,
  height: 6,
  background: "var(--muted-foreground)",
  border: "none",
  minWidth: 0,
  minHeight: 0,
};

function ToolkitChips({ toolkits }: { toolkits: string[] }) {
  return (
    <div style={{ display: "flex", gap: 4, marginTop: 6, flexWrap: "wrap" }}>
      {toolkits.slice(0, 6).map((slug) => {
        const logo = toolkitLogo(slug);
        return (
          <span
            key={slug}
            title={slug}
            style={{
              width: 18,
              height: 18,
              borderRadius: 5,
              background: "var(--muted)",
              border: "1px solid var(--border)",
              display: "inline-flex",
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            {logo ? (
              // eslint-disable-next-line @next/next/no-img-element
              <img src={logo} alt={slug} style={{ width: 11, height: 11 }} />
            ) : (
              <span
                style={{
                  fontSize: 7,
                  fontWeight: 700,
                  color: "var(--muted-foreground)",
                }}
              >
                {toolkitInitials(slug)}
              </span>
            )}
          </span>
        );
      })}
    </div>
  );
}

// --- custom nodes ---------------------------------------------------------------

function triggerIcon(kind: string): string {
  if (kind === "schedule") return "🕐";
  if (kind === "composio") return "✉️";
  if (kind === "chat") return "💬";
  if (kind === "webhook") return "🔗";
  return "▶️";
}

function TriggerNode({ data }: NodeProps) {
  const d = data as unknown as TriggerNodeData;
  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "center" }}>
      <div
        style={{
          fontSize: 11,
          fontWeight: 700,
          color: "#b45309",
          background: "#fef3c7",
          border: "1px solid #fde68a",
          borderRadius: 99,
          padding: "3px 12px",
          marginBottom: 8,
        }}
      >
        ⚡ Trigger
      </div>
      <div
        style={{
          ...pillCard,
          borderRadius: 18,
          display: "flex",
          alignItems: "center",
          gap: 9,
          border: d.active ? `1.5px solid ${ACCENT}` : "1px solid var(--border)",
          position: "relative",
        }}
      >
        {d.logo ? (
          // eslint-disable-next-line @next/next/no-img-element
          <img src={d.logo} alt="" style={{ width: 16, height: 16 }} />
        ) : (
          <span style={{ fontSize: 15 }}>{triggerIcon(d.kind)}</span>
        )}
        <span
          style={{
            fontSize: 14,
            fontWeight: 600,
            color: "var(--foreground)",
            whiteSpace: "nowrap",
          }}
        >
          {d.label}
        </span>
        <Handle type="source" position={Position.Bottom} style={handleDot} />
      </div>
    </div>
  );
}

function AgentNode({ data }: NodeProps) {
  const d = data as unknown as AgentNodeData;
  const a = d.agent;
  const border = d.live
    ? `1.5px solid ${ACCENT}`
    : d.outcome === "ok"
      ? "1.5px solid var(--status-completed)"
      : d.outcome === "error"
        ? "1.5px solid var(--destructive)"
        : "1px solid var(--border)";

  const [hovered, setHovered] = useState(false);
  const [prompt, setPrompt] = useState("");
  const [sending, setSending] = useState(false);
  const [reply, setReply] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  // Keep the bar open while the user is interacting or a reply is showing.
  const open = hovered || prompt.length > 0 || reply !== null || sending;

  const send = useCallback(async () => {
    const text = prompt.trim();
    if (!text || sending) return;
    setSending(true);
    setErr(null);
    try {
      const res = await fetch(
        `/api/ui/agents?userId=${encodeURIComponent(d.userId)}`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ op: "ask", agentId: d.agentId, prompt: text }),
        }
      );
      const json = (await res.json().catch(() => null)) as
        | { ok?: boolean; text?: string; error?: string }
        | null;
      if (!res.ok || !json?.ok) {
        setErr(json?.error ?? `error ${res.status}`);
      } else {
        setReply(json.text ?? "");
        setPrompt("");
      }
    } catch (e: any) {
      setErr(String(e?.message ?? e).slice(0, 160));
    } finally {
      setSending(false);
    }
  }, [prompt, sending, d.userId, d.agentId]);

  return (
    <div
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      style={{
        ...pillCard,
        border,
        minWidth: open ? 256 : 175,
        width: open ? 256 : undefined,
        position: "relative",
        transition: "min-width 120ms ease, width 120ms ease",
        cursor: "grab",
      }}
    >
      <Handle type="target" position={Position.Top} style={handleDot} />
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <div style={{ position: "relative" }}>
          <PixelAvatar seed={`${d.agentId}:${a?.name ?? ""}`} size={34} />
          {d.live && (
            <span
              style={{
                position: "absolute",
                top: -3,
                right: -3,
                width: 9,
                height: 9,
                borderRadius: 99,
                background: ACCENT,
                border: "2px solid var(--card)",
              }}
            />
          )}
        </div>
        <div style={{ minWidth: 0 }}>
          <div
            style={{
              fontSize: 14,
              fontWeight: 600,
              color: "var(--foreground)",
              whiteSpace: "nowrap",
              overflow: "hidden",
              textOverflow: "ellipsis",
              maxWidth: 200,
            }}
          >
            {a?.name ?? d.agentId}
          </div>
          {d.botUsername && (
            <div style={{ fontSize: 10.5, color: ACCENT }}>@{d.botUsername}</div>
          )}
          <ToolkitChips toolkits={a?.toolkits ?? []} />
        </div>
      </div>

      {(reply !== null || err) && (
        <div
          className="nodrag nowheel"
          style={{
            marginTop: 8,
            maxHeight: 168,
            overflowY: "auto",
            fontSize: 11.5,
            lineHeight: 1.45,
            color: err ? "var(--destructive)" : "var(--foreground)",
            background: "var(--muted)",
            border: "1px solid var(--border)",
            borderRadius: 10,
            padding: "8px 10px",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
          }}
        >
          {err ?? reply}
        </div>
      )}

      {open && (
        <div
          className="nodrag nowheel"
          style={{
            marginTop: 8,
            display: "flex",
            alignItems: "center",
            gap: 6,
            background: "var(--background)",
            border: "1px solid var(--border)",
            borderRadius: 999,
            padding: "3px 3px 3px 12px",
          }}
        >
          <input
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void send();
              }
            }}
            placeholder={sending ? "Thinking…" : "Ask this agent…"}
            disabled={sending}
            autoFocus
            style={{
              flex: 1,
              minWidth: 0,
              border: "none",
              outline: "none",
              background: "transparent",
              fontSize: 12,
              color: "var(--foreground)",
            }}
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={sending || !prompt.trim()}
            title="Send"
            style={{
              width: 26,
              height: 26,
              flex: "0 0 auto",
              borderRadius: 999,
              border: "none",
              cursor: sending || !prompt.trim() ? "default" : "pointer",
              background:
                sending || !prompt.trim() ? "var(--muted)" : ACCENT,
              color: "#fff",
              display: "inline-flex",
              alignItems: "center",
              justifyContent: "center",
              fontSize: 13,
              lineHeight: 1,
            }}
          >
            {sending ? "…" : "↑"}
          </button>
        </div>
      )}

      <Handle type="source" position={Position.Bottom} style={handleDot} />
    </div>
  );
}

function RouteNode({ data }: NodeProps) {
  const d = data as unknown as RouteNodeData;
  return (
    <div
      style={{
        ...pillCard,
        borderRadius: 14,
        border: d.live ? `1.5px dashed ${ACCENT}` : "1.5px dashed var(--border)",
        background: "var(--card)",
        display: "flex",
        alignItems: "center",
        gap: 8,
        padding: "8px 14px",
        position: "relative",
      }}
      title={`${d.instruction}\npicks from: ${d.candidates.join(", ")}`}
    >
      <Handle type="target" position={Position.Top} style={handleDot} />
      <span style={{ fontSize: 13 }}>⚡</span>
      <span style={{ fontSize: 13, color: "var(--muted-foreground)", whiteSpace: "nowrap" }}>
        AI routing
      </span>
      {d.picked.length > 0 && (
        <span style={{ fontSize: 11, color: "var(--status-completed)" }}>
          → {d.picked.join(", ")}
        </span>
      )}
      <Handle type="source" position={Position.Bottom} style={handleDot} />
    </div>
  );
}

const nodeTypes = {
  trigger: TriggerNode,
  agent: AgentNode,
  route: RouteNode,
};

// Smoothstep edge with dots flowing along the path (SVG animateMotion). Live
// edges run faster, bigger and indigo; idle edges drift a single grey dot.
function DotFlowEdge(props: EdgeProps) {
  const {
    id,
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    style,
  } = props;
  const [edgePath] = getSmoothStepPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
    borderRadius: 14,
  });
  const live = Boolean((props.data as { live?: boolean } | undefined)?.live);
  // Slow on purpose. One direction, constant velocity (animateMotion's default
  // "paced" mode) → no easing jerk; an opacity fade at each end hides the loop
  // reset, so it reads as a single subtle pulse drifting down the wire instead
  // of a badge snapping back to the start.
  const dur = live ? 5 : 7;
  const dotted: CSSProperties = {
    ...style,
    stroke: "var(--canvas-edge)",
    strokeWidth: live ? 2.4 : 1.8,
    strokeDasharray: "0.1 9",
    strokeLinecap: "round",
  };
  return (
    <>
      <BaseEdge id={id} path={edgePath} style={dotted} />
      <g>
        <rect x={-11} y={-7} width={22} height={14} rx={7} fill="#22c55e" />
        <text
          x={0}
          y={0.5}
          textAnchor="middle"
          dominantBaseline="central"
          fontSize={8}
          fontWeight={600}
          fill="#ffffff"
        >
          200
        </text>
        <animateMotion
          dur={`${dur}s`}
          repeatCount="indefinite"
          path={edgePath}
          rotate="0"
        />
        <animate
          attributeName="opacity"
          dur={`${dur}s`}
          repeatCount="indefinite"
          values="0;0.5;0.5;0"
          keyTimes="0;0.18;0.82;1"
        />
      </g>
    </>
  );
}

const edgeTypes = {
  dots: DotFlowEdge,
};

const AGENTS_TAB = "__agents__";

// --- canvas -------------------------------------------------------------------

export default function WorkforceCanvas({
  userId,
  agents,
  teams,
  bots,
}: {
  userId: string;
  agents: CanvasAgent[];
  teams: CanvasTeam[];
  bots: CanvasBot[];
}) {
  const [selectedTeamId, setSelectedTeamId] = useState<string>(
    teams[0]?.id ?? AGENTS_TAB
  );
  const [selectedNode, setSelectedNode] = useState<
    { kind: "agent"; agentId: string } | { kind: "team" } | null
  >(null);
  const [runs, setRuns] = useState<RunListItem[]>([]);
  const [runDetail, setRunDetail] = useState<RunDetail | null>(null);
  const [showNew, setShowNew] = useState(false);
  const [teamView, setTeamView] = useState<"canvas" | "office" | "activity">(
    "canvas"
  );
  // The `agents` prop is server-loaded and not refetched; model changes made
  // through the picker are reflected locally so the UI updates without a reload.
  const [modelOverrides, setModelOverrides] = useState<Record<string, string | null>>(
    {}
  );
  const [editModelAgentId, setEditModelAgentId] = useState<string | null>(null);
  const [showcaseAgentId, setShowcaseAgentId] = useState<string | null>(null);
  const modelFor = useCallback(
    (a: CanvasAgent): string | null =>
      a.id in modelOverrides ? modelOverrides[a.id] : a.modelName,
    [modelOverrides]
  );

  const team = teams.find((t) => t.id === selectedTeamId) ?? null;
  const agentById = useMemo(() => {
    const m = new Map<string, CanvasAgent>();
    for (const a of agents) m.set(a.id, a);
    return m;
  }, [agents]);
  const botByAgent = useMemo(() => {
    const m = new Map<string, CanvasBot>();
    for (const b of bots) m.set(b.agentId, b);
    return m;
  }, [bots]);

  // --- run polling ------------------------------------------------------------

  const refreshRuns = useCallback(async () => {
    if (!team) return;
    try {
      const res = await fetch(
        `/api/ui/agents?op=runs&teamId=${encodeURIComponent(team.id)}&userId=${encodeURIComponent(userId)}`,
        { cache: "no-store" }
      );
      if (!res.ok) return;
      const data = (await res.json()) as { runs: RunListItem[] };
      setRuns(data.runs ?? []);
      const latest = (data.runs ?? [])[0];
      if (latest) {
        const detRes = await fetch(
          `/api/ui/agents?op=run&runId=${encodeURIComponent(latest.id)}&userId=${encodeURIComponent(userId)}`,
          { cache: "no-store" }
        );
        if (detRes.ok) setRunDetail((await detRes.json()) as RunDetail);
      } else {
        setRunDetail(null);
      }
    } catch {
      // transient poll failure — keep last state
    }
  }, [team, userId]);

  useEffect(() => {
    setRuns([]);
    setRunDetail(null);
    void refreshRuns();
    const id = setInterval(() => void refreshRuns(), 6000);
    return () => clearInterval(id);
  }, [refreshRuns]);

  // --- derive live-run state ----------------------------------------------------

  const latestRun = runs[0] ?? null;
  const isLive = latestRun?.status === "running";
  // Memoized: a fresh `[]` here each render cascades through outcomeFor →
  // the nodes useMemo → the flowNodes sync effect → setState → re-render —
  // an infinite loop (React #185) that crashed the whole page.
  const stageRecords = useMemo(() => runDetail?.stages ?? [], [runDetail]);
  // Records append as stages FINISH, so during a live run the active stage
  // index is the number of recorded stages.
  const liveStageIndex = isLive ? stageRecords.length : -1;

  const outcomeFor = useCallback(
    (stageIndex: number, agentId: string): "ok" | "error" | null => {
      const rec = stageRecords.find((r) => r.stageIndex === stageIndex);
      const out = rec?.outputs.find((o) => o.agentId === agentId);
      return out ? out.status : null;
    },
    [stageRecords]
  );

  // --- build nodes/edges -----------------------------------------------------------

  const { nodes, edges } = useMemo(() => {
    const nodes: Node[] = [];
    const edges: Edge[] = [];
    if (!team) return { nodes, edges };

    // Vertical flow like the reference: trigger on top, stages flow downward;
    // members of a stage spread horizontally.
    nodes.push({
      id: "trigger",
      type: "trigger",
      position: { x: 0, y: 0 },
      data: {
        label: team.triggerLabel,
        kind: team.triggerKind ?? "manual",
        active: isLive,
        logo: team.triggerLogo ?? null,
      } satisfies TriggerNodeData,
    });

    let prevIds: string[] = ["trigger"];

    team.stages.forEach((stage, i) => {
      const y = (i + 1) * 190 + 40;
      const colIds: string[] = [];

      if (stage.kind === "route") {
        const rec = stageRecords.find((r) => r.stageIndex === i);
        const id = `stage-${i}-route`;
        nodes.push({
          id,
          type: "route",
          position: { x: 0, y },
          data: {
            instruction: stage.instruction,
            candidates: stage.candidateAgentIds.map(
              (aid) => agentById.get(aid)?.name ?? aid
            ),
            picked: (rec?.pickedAgentIds ?? []).map(
              (aid) => agentById.get(aid)?.name ?? aid
            ),
            stageIndex: i,
            live: liveStageIndex === i,
          } satisfies RouteNodeData,
        });
        colIds.push(id);
      } else {
        const members = stage.agentIds;
        members.forEach((agentId, idx) => {
          const id = `agent-${i}-${agentId}`;
          const x = (idx - (members.length - 1) / 2) * COL_W;
          nodes.push({
            id,
            type: "agent",
            position: { x, y },
            data: {
              agent: agentById.get(agentId) ?? null,
              agentId,
              botUsername: botByAgent.get(agentId)?.username ?? null,
              stageIndex: i,
              live: liveStageIndex === i,
              outcome: outcomeFor(i, agentId),
              userId,
            } satisfies AgentNodeData,
          });
          colIds.push(id);
        });
      }

      const liveEdge = liveStageIndex === i;
      for (const from of prevIds) {
        for (const to of colIds) {
          edges.push({
            id: `e-${from}-${to}`,
            source: from,
            target: to,
            type: "dots",
            data: { live: liveEdge },
            style: liveEdge
              ? { stroke: "var(--canvas-edge)", strokeWidth: 2.4 }
              : { stroke: "var(--canvas-edge)", strokeWidth: 1.8 },
          });
        }
      }
      prevIds = colIds;
    });

    return { nodes, edges };
  }, [team, agentById, botByAgent, isLive, liveStageIndex, stageRecords, outcomeFor, userId]);

  // Draggable nodes: ReactFlow needs controlled node state + onNodesChange.
  // Layout/data recomputes (poll ticks, live highlights) must NOT snap nodes
  // back, so user-dragged positions are preserved by id — except on team
  // switch, where the layout starts fresh.
  const [flowNodes, setFlowNodes] = useState<Node[]>(nodes);
  const lastTeamRef = useRef(selectedTeamId);
  useEffect(() => {
    if (lastTeamRef.current !== selectedTeamId) {
      lastTeamRef.current = selectedTeamId;
      setFlowNodes(nodes);
      return;
    }
    setFlowNodes((prev) => {
      const posById = new Map(prev.map((n) => [n.id, n.position] as const));
      return nodes.map((n) => ({ ...n, position: posById.get(n.id) ?? n.position }));
    });
  }, [nodes, selectedTeamId]);
  const onNodesChange = useCallback<OnNodesChange>(
    (changes) => setFlowNodes((nds) => applyNodeChanges(changes, nds)),
    []
  );

  const onNodeClick = useCallback((_: unknown, node: Node) => {
    if (node.type === "agent") {
      const d = node.data as unknown as AgentNodeData;
      setSelectedNode({ kind: "agent", agentId: d.agentId });
    } else {
      setSelectedNode({ kind: "team" });
    }
  }, []);

  // --- side panel content ----------------------------------------------------------

  const panelAgent =
    selectedNode?.kind === "agent"
      ? agentById.get(selectedNode.agentId) ?? null
      : null;
  const panelAgentOutput = (() => {
    if (!panelAgent) return null;
    for (let i = stageRecords.length - 1; i >= 0; i--) {
      const out = stageRecords[i]!.outputs.find((o) => o.agentId === panelAgent.id);
      if (out) return out;
    }
    return null;
  })();

  // Live footer narration: who is working right now.
  const liveLabel = (() => {
    if (!team || !isLive) return null;
    const stage = team.stages[liveStageIndex];
    if (!stage) return `Run ${latestRun?.id} in progress…`;
    if (stage.kind === "route") return "AI routing — picking agents…";
    const names = stage.agentIds
      .map((id) => agentById.get(id)?.name ?? id)
      .join(", ");
    return `Stage ${liveStageIndex + 1} of ${team.stages.length} — ${names} working…`;
  })();

  // --- render ----------------------------------------------------------------------

  const tabs: Array<{ id: string; label: string }> = [
    ...teams.map((t) => ({ id: t.id, label: `${t.emoji ?? "🤝"} ${t.name}` })),
    { id: AGENTS_TAB, label: "All agents" },
  ];

  return (
    <div>
      {/* tab strip */}
      <div
        style={{
          display: "flex",
          gap: 4,
          marginBottom: 0,
          flexWrap: "wrap",
          borderBottom: "1px solid var(--border)",
        }}
      >
        {tabs.map((t) => {
          const active = t.id === selectedTeamId;
          const paused =
            t.id !== AGENTS_TAB && !teams.find((x) => x.id === t.id)?.enabled;
          return (
            <button
              key={t.id}
              onClick={() => {
                setSelectedTeamId(t.id);
                setSelectedNode(null);
              }}
              style={{
                fontSize: 14,
                fontWeight: 600,
                color: active ? "#6366f1" : "var(--muted-foreground)",
                background: "transparent",
                border: "none",
                borderBottom: active
                  ? "2.5px solid #6366f1"
                  : "2.5px solid transparent",
                padding: "10px 16px",
                cursor: "pointer",
              }}
            >
              {t.label}
              {paused && (
                <span style={{ marginLeft: 6, fontSize: 10, fontWeight: 500 }}>
                  ⏸
                </span>
              )}
            </button>
          );
        })}
        {selectedTeamId !== AGENTS_TAB && (
          <div
            style={{
              display: "flex",
              gap: 2,
              margin: "6px 8px 6px auto",
              padding: 2,
              borderRadius: 99,
              background: "var(--muted)",
              border: "1px solid var(--border)",
            }}
          >
            {(["canvas", "office", "activity"] as const).map((v) => (
              <button
                key={v}
                onClick={() => setTeamView(v)}
                style={{
                  fontSize: 12,
                  fontWeight: 600,
                  border: "none",
                  borderRadius: 99,
                  padding: "4px 12px",
                  cursor: "pointer",
                  color: teamView === v ? "#fff" : "var(--muted-foreground)",
                  background: teamView === v ? "#6366f1" : "transparent",
                }}
              >
                {v === "canvas" ? "⛓ Flow" : v === "office" ? "🏠 Office" : "📊 Activity"}
              </button>
            ))}
          </div>
        )}
        <button
          onClick={() => setShowNew(true)}
          style={{
            fontSize: 13,
            fontWeight: 600,
            color: "#6366f1",
            background: "transparent",
            border: "1px dashed #6366f1",
            borderRadius: 99,
            padding: "5px 14px",
            margin: selectedTeamId !== AGENTS_TAB ? "6px 0" : "6px 0 6px auto",
            cursor: "pointer",
          }}
        >
          + New workforce
        </button>
      </div>

      {showNew && (
        <NewWorkforceModal userId={userId} onClose={() => setShowNew(false)} />
      )}

      {selectedTeamId === AGENTS_TAB ? (
        // --- roster: every agent as its own profile card -----------------------
        <div
          style={{
            background: "var(--card)",
            border: "1px solid var(--border)",
            borderTop: "none",
            borderRadius: "0 0 12px 12px",
            padding: 24,
            display: "flex",
            flexWrap: "wrap",
            gap: 14,
          }}
        >
          {agents.length === 0 && (
            <p style={{ fontSize: 13, color: "var(--muted-foreground)" }}>
              No agents yet. Create one in chat:{" "}
              <code>/agent create Scout | toolkits: gmail | persona: triages my inbox</code>
            </p>
          )}
          {agents.map((a) => {
            const bot = botByAgent.get(a.id);
            return (
              <div
                key={a.id}
                onClick={() => setShowcaseAgentId(a.id)}
                style={{ ...pillCard, width: 250, padding: 16, cursor: "pointer" }}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                  <PixelAvatar seed={`${a.id}:${a.name}`} size={44} />
                  <div style={{ minWidth: 0 }}>
                    <div
                      style={{
                        fontSize: 15,
                        fontWeight: 600,
                        color: "var(--foreground)",
                      }}
                    >
                      {a.name}
                    </div>
                    {bot ? (
                      <div style={{ fontSize: 11, color: ACCENT }}>
                        @{bot.username}
                      </div>
                    ) : (
                      <div style={{ fontSize: 11, color: "var(--muted-foreground)" }}>
                        {a.id}
                      </div>
                    )}
                  </div>
                </div>
                <p
                  style={{
                    fontSize: 12,
                    color: "var(--muted-foreground)",
                    lineHeight: 1.5,
                    margin: "10px 0 0",
                    display: "-webkit-box",
                    WebkitLineClamp: 3,
                    WebkitBoxOrient: "vertical",
                    overflow: "hidden",
                  }}
                >
                  {a.persona || "—"}
                </p>
                <ToolkitChips toolkits={a.toolkits} />
                <ModelChip
                  modelName={modelFor(a)}
                  editable={!a.id.startsWith("ag_hc_")}
                  onClick={(e) => {
                    e.stopPropagation();
                    setEditModelAgentId(a.id);
                  }}
                />
              </div>
            );
          })}
        </div>
      ) : teamView === "office" ? (
        <WorkforceOffice
          userId={userId}
          teamId={selectedTeamId}
          teamName={team ? `${team.emoji ?? "🤝"} ${team.name}` : ""}
          live={isLive}
          liveStageIndex={liveStageIndex}
        />
      ) : teamView === "activity" ? (
        <ActivityDashboard userId={userId} teamId={selectedTeamId} />
      ) : (
        // --- workflow canvas + side panel ----------------------------------------
        <div style={{ display: "flex", alignItems: "stretch" }}>
          <div
            style={{
              flex: 1,
              minWidth: 0,
              height: 580,
              background: "var(--card)",
              border: "1px solid var(--border)",
              borderTop: "none",
              borderRadius: "0 0 0 12px",
              overflow: "hidden",
              position: "relative",
              display: "flex",
              flexDirection: "column",
            }}
          >
            <div style={{ flex: 1, minHeight: 0 }}>
              <ReactFlow
                nodes={flowNodes}
                edges={edges}
                nodeTypes={nodeTypes}
                edgeTypes={edgeTypes}
                onNodesChange={onNodesChange}
                onNodeClick={onNodeClick}
                nodesDraggable
                fitView
                fitViewOptions={{ padding: 0.3, maxZoom: 1 }}
                proOptions={{ hideAttribution: true }}
              >
                <Background
                  variant={BackgroundVariant.Dots}
                  gap={18}
                  size={1.6}
                  // concrete hex — the dot pattern fill is an SVG attribute
                  color="#b4b9c6"
                />
                <Controls showInteractive={false} />
              </ReactFlow>
            </div>
            {/* live footer */}
            <div
              style={{
                borderTop: "1px solid var(--border)",
                padding: "10px 16px",
                fontSize: 12.5,
                color: liveLabel ? "#6366f1" : "var(--muted-foreground)",
                display: "flex",
                alignItems: "center",
                gap: 8,
                background: "var(--card)",
              }}
            >
              {liveLabel ? (
                <>
                  <span
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: 99,
                      background: "#6366f1",
                      animation: "pulse 1.4s ease-in-out infinite",
                    }}
                  />
                  {liveLabel}
                </>
              ) : latestRun ? (
                <>
                  <span
                    style={{
                      width: 8,
                      height: 8,
                      borderRadius: 99,
                      background:
                        latestRun.status === "ok"
                          ? "var(--status-completed)"
                          : "var(--destructive)",
                    }}
                  />
                  Last run {latestRun.status} ·{" "}
                  {new Date(latestRun.startedAt)
                    .toISOString()
                    .replace("T", " ")
                    .slice(5, 16)}
                </>
              ) : (
                <>Idle — fire with /team run {team?.id}</>
              )}
            </div>
            <style>{`@keyframes pulse { 0%,100% { opacity: 1 } 50% { opacity: 0.3 } }`}</style>
          </div>

          {/* side panel */}
          <aside style={panelStyles.panel}>
            {panelAgent ? (
              <>
                <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                  <PixelAvatar seed={`${panelAgent.id}:${panelAgent.name}`} size={44} />
                  <div>
                    <div
                      style={{ fontSize: 17, fontWeight: 600, color: "var(--foreground)" }}
                    >
                      {panelAgent.name}
                    </div>
                    {botByAgent.get(panelAgent.id) && (
                      <div style={{ fontSize: 12, color: ACCENT }}>
                        @{botByAgent.get(panelAgent.id)!.username} on Telegram
                      </div>
                    )}
                  </div>
                </div>
                <div style={panelStyles.label}>Persona</div>
                <p style={panelStyles.body}>{panelAgent.persona || "—"}</p>
                <div style={panelStyles.label}>Toolkits</div>
                <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                  {panelAgent.toolkits.length ? (
                    panelAgent.toolkits.map((slug) => (
                      <code key={slug} style={panelStyles.chip}>
                        {slug}
                      </code>
                    ))
                  ) : (
                    <span style={{ fontSize: 12, color: "var(--muted-foreground)" }}>
                      pure reasoning
                    </span>
                  )}
                </div>
                {panelAgent.skills?.length ? (
                  <>
                    <div style={panelStyles.label}>Skills</div>
                    <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                      {panelAgent.skills.map((s) => (
                        <code key={s} style={panelStyles.chip}>
                          {s}
                        </code>
                      ))}
                    </div>
                  </>
                ) : null}
                <div style={panelStyles.label}>AI model</div>
                <ModelChip
                  modelName={modelFor(panelAgent)}
                  editable={!panelAgent.id.startsWith("ag_hc_")}
                  onClick={() => setEditModelAgentId(panelAgent.id)}
                />
                {panelAgentOutput && (
                  <>
                    <div style={panelStyles.label}>
                      Latest output ({panelAgentOutput.status})
                    </div>
                    <p style={{ ...panelStyles.body, whiteSpace: "pre-wrap" }}>
                      {panelAgentOutput.text.length > 1200
                        ? panelAgentOutput.text.slice(0, 1200) + "…"
                        : panelAgentOutput.text}
                    </p>
                  </>
                )}
              </>
            ) : team ? (
              <>
                <div style={{ fontSize: 17, fontWeight: 600, color: "var(--foreground)" }}>
                  {team.emoji ?? "🤝"} {team.name}
                </div>
                <div style={{ fontSize: 12, color: "var(--muted-foreground)", marginTop: 3 }}>
                  {team.triggerLabel} · {team.enabled ? "active" : "paused"}
                </div>
                <div style={panelStyles.label}>Mission</div>
                <p style={panelStyles.body}>{team.spec}</p>
                {team.chatLog && team.chatLog.length > 0 && (
                  <>
                    <div style={panelStyles.label}>Chat logs</div>
                    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                      {team.chatLog.map((c, i) => (
                        <div key={i} style={panelStyles.chatRow}>
                          <span style={panelStyles.chatTag}>
                            {c.group ? "GROUP" : "DM"}
                          </span>
                          <div style={{ minWidth: 0 }}>
                            <div style={panelStyles.chatWho}>{c.who}</div>
                            <div style={panelStyles.chatText}>{c.text}</div>
                          </div>
                        </div>
                      ))}
                    </div>
                  </>
                )}
                <div style={panelStyles.label}>Recent runs</div>
                {runs.length === 0 ? (
                  <p style={panelStyles.body}>
                    No runs yet. Fire one with <code>/team run {team.id}</code>.
                  </p>
                ) : (
                  <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                    {runs.slice(0, 8).map((r) => (
                      <div key={r.id} style={panelStyles.runRow}>
                        <span
                          style={{
                            width: 8,
                            height: 8,
                            borderRadius: 99,
                            flexShrink: 0,
                            background:
                              r.status === "running"
                                ? ACCENT
                                : r.status === "ok"
                                  ? "var(--status-completed)"
                                  : "var(--destructive)",
                          }}
                        />
                        <span style={{ fontSize: 12, color: "var(--foreground)" }}>
                          {r.source}
                        </span>
                        <span style={{ fontSize: 11, color: "var(--muted-foreground)" }}>
                          {new Date(r.startedAt)
                            .toISOString()
                            .replace("T", " ")
                            .slice(5, 16)}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
                {latestRun?.resultText && (
                  <>
                    <div style={panelStyles.label}>Latest summary</div>
                    <p style={{ ...panelStyles.body, whiteSpace: "pre-wrap" }}>
                      {latestRun.resultText.length > 1200
                        ? latestRun.resultText.slice(0, 1200) + "…"
                        : latestRun.resultText}
                    </p>
                  </>
                )}
              </>
            ) : null}
          </aside>
        </div>
      )}

      {editModelAgentId &&
        (() => {
          const a = agentById.get(editModelAgentId);
          if (!a) return null;
          return (
            <ModelPickerModal
              userId={userId}
              agentId={a.id}
              agentName={a.name}
              current={modelFor(a)}
              onClose={() => setEditModelAgentId(null)}
              onSaved={(modelName) => {
                setModelOverrides((prev) => ({ ...prev, [a.id]: modelName }));
                setEditModelAgentId(null);
              }}
            />
          );
        })()}

      {showcaseAgentId &&
        (() => {
          const a = agentById.get(showcaseAgentId);
          if (!a) return null;
          return (
            <AgentShowcase
              agent={a}
              botUsername={botByAgent.get(a.id)?.username ?? null}
              modelName={modelFor(a)}
              onClose={() => setShowcaseAgentId(null)}
            />
          );
        })()}
    </div>
  );
}

function ModelChip({
  modelName,
  onClick,
  editable = true,
}: {
  modelName: string | null;
  onClick: (e: ReactMouseEvent) => void;
  editable?: boolean;
}) {
  const info = modelName ? getModelInfo(modelName) : undefined;
  const label = info ? info.label : modelName ? modelName : "Default model";
  return (
    <button
      type="button"
      onClick={editable ? onClick : undefined}
      disabled={!editable}
      style={{
        marginTop: 10,
        display: "flex",
        alignItems: "center",
        gap: 8,
        width: "100%",
        padding: "7px 10px",
        borderRadius: 8,
        border: "1px solid var(--border)",
        background: "var(--muted)",
        color: "var(--foreground)",
        fontSize: 12,
        fontWeight: 500,
        cursor: editable ? "pointer" : "default",
        textAlign: "left",
      }}
    >
      <span aria-hidden style={{ fontSize: 13 }}>
        🧠
      </span>
      <span style={{ flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        {label}
      </span>
      {info && (
        <span style={{ fontSize: 10, color: "var(--muted-foreground)" }}>
          {info.vendor}
        </span>
      )}
      {editable && <span style={{ fontSize: 11, color: ACCENT }}>Change</span>}
    </button>
  );
}

const panelStyles: Record<string, CSSProperties> = {
  panel: {
    width: 300,
    flexShrink: 0,
    background: "var(--card)",
    border: "1px solid var(--border)",
    borderTop: "none",
    borderLeft: "none",
    borderRadius: "0 0 12px 0",
    padding: 18,
    overflowY: "auto",
    maxHeight: 580,
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
  body: {
    fontSize: 13,
    color: "var(--foreground)",
    lineHeight: 1.55,
    margin: 0,
  },
  chip: {
    fontSize: 11,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 7,
    padding: "2px 7px",
  },
  runRow: {
    display: "flex",
    alignItems: "center",
    gap: 8,
  },
  chatRow: {
    display: "flex",
    gap: 8,
    alignItems: "flex-start",
  },
  chatTag: {
    flex: "0 0 auto",
    fontSize: 9,
    fontWeight: 700,
    letterSpacing: 0.4,
    color: "var(--muted-foreground)",
    background: "var(--muted)",
    border: "1px solid var(--border)",
    borderRadius: 6,
    padding: "2px 5px",
    marginTop: 1,
  },
  chatWho: {
    fontSize: 12,
    fontWeight: 600,
    color: "var(--foreground)",
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  chatText: {
    fontSize: 11.5,
    color: "var(--muted-foreground)",
    lineHeight: 1.4,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
};
