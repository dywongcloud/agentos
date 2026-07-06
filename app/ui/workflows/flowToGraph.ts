// app/ui/workflows/flowToGraph.ts
//
// RUNTIME port of scripts/nbriWorkflows.mjs `buildGraph` — translates a
// presentation-only `Flow` into the WDK manifest's `{ nodes, edges }` graph
// vocabulary so workforce diagrams can be injected into the live dashboard
// manifest object WITHOUT a rebuild. The build script handles the static NBRI
// flows at build time; this handles user-created workforces at request time.
//
// WDK graph vocabulary (reverse-engineered, must match the dashboard's viewer):
//   node.type:  workflowStart | step | primitive | conditional | workflowEnd
//   edge.type:  default | conditional (label=branch) | loop | parallel
//   metadata:   loopId | conditionalId | conditionalBranch | hook

import type { Flow, FlowNode } from "./nbriFlows";

export type WdkNode = {
  id: string;
  type: string;
  data: { label: string; nodeKind: string };
  metadata?: Record<string, unknown>;
};
export type WdkEdge = {
  id: string;
  source: string;
  target: string;
  type: string;
  label?: string;
};
export type WdkGraph = { nodes: WdkNode[]; edges: WdkEdge[] };

const A = (s?: string) => (s ? ` [${s.toUpperCase()}]` : "");
const ACT = (s?: string) => (s ? ` · ${s}` : "");

function nodeLabel(n: FlowNode): string {
  switch (n.kind) {
    case "trigger":
      return `▶ ${n.label}${ACT(n.actor)}`;
    case "step":
      return `${n.label}${ACT(n.actor)}${A(n.auto)}`;
    case "hook":
      return `⏸ ${n.label}${n.detail ? ` (${n.detail})` : ""}${ACT(n.actor)}`;
    case "sleep":
      return `⏱ ${n.label}${n.detail ? ` — ${n.detail}` : ""}`;
    case "loop":
      return `↺ ${n.label}`;
    default:
      return n.label ?? "";
  }
}
function laneLabel(l: { label: string; detail?: string; actor?: string; auto?: string }): string {
  return `${l.label}${ACT(l.actor)}${A(l.auto)}`;
}

export function flowToGraph(flow: Flow): WdkGraph {
  const nodes: WdkNode[] = [];
  const edges: WdkEdge[] = [];
  const hookIds = new Set<string>();
  let nId = 0;
  let cId = 0;
  let lId = 0;
  let eId = 0;

  const newNode = (
    type: string,
    label: string,
    nodeKind: string,
    metadata?: Record<string, unknown>
  ): string => {
    const id = `node_${nId++}`;
    const node: WdkNode = { id, type, data: { label, nodeKind } };
    if (metadata) node.metadata = metadata;
    nodes.push(node);
    return id;
  };
  const addEdge = (source: string, target: string, type: string, label?: string) => {
    const e: WdkEdge = { id: `e_${eId++}`, source, target, type };
    if (label) e.label = label;
    edges.push(e);
  };

  nodes.push({
    id: "start",
    type: "workflowStart",
    data: { label: `Start: ${flow.title}`, nodeKind: "workflow_start" },
  });
  const endNodeId = "end";

  type SubGraph = { entries: string[]; exits: string[] };

  function buildNode(fn: FlowNode): SubGraph {
    switch (fn.kind) {
      case "trigger":
      case "sleep": {
        const id = newNode("primitive", nodeLabel(fn), "primitive");
        return { entries: [id], exits: [id] };
      }
      case "step": {
        const id = newNode("step", nodeLabel(fn), "step");
        return { entries: [id], exits: [id] };
      }
      case "hook": {
        const id = newNode("primitive", nodeLabel(fn), "primitive", { hook: true });
        hookIds.add(id);
        return { entries: [id], exits: [id] };
      }
      case "loop": {
        const loopId = `loop_${lId++}`;
        const id = newNode("primitive", nodeLabel(fn), "primitive", { loopId });
        addEdge(id, "__BODY_FIRST__", "loop");
        return { entries: [id], exits: [] };
      }
      case "parallel": {
        const laneIds = fn.lanes.map((l) =>
          newNode("primitive", laneLabel(l), "primitive")
        );
        return { entries: laneIds, exits: laneIds };
      }
      case "decision": {
        const condId = `cond_${cId++}`;
        const cnode = newNode("conditional", fn.label, "conditional", {
          conditionalId: condId,
        });
        const exits: string[] = [];
        for (const br of fn.branches) {
          const sub = buildSeq(br.nodes, condId, br.label);
          for (const en of sub.entries) addEdge(cnode, en, "conditional", br.label);
          exits.push(...sub.exits);
        }
        return { entries: [cnode], exits };
      }
      default:
        return { entries: [], exits: [] };
    }
  }

  function buildSeq(seq: FlowNode[], condId?: string, condBranch?: string): SubGraph {
    let seqEntries: string[] | null = null;
    let prevExits: string[] | null = null;
    for (const fn of seq) {
      const sub = buildNode(fn);
      if (condId) {
        const tagged = new Set([...sub.entries, ...sub.exits]);
        for (const n of nodes) {
          if (tagged.has(n.id)) {
            n.metadata = {
              ...(n.metadata || {}),
              conditionalId: condId,
              conditionalBranch: condBranch,
            };
          }
        }
      }
      if (seqEntries === null) seqEntries = sub.entries;
      if (prevExits) {
        for (const pe of prevExits) for (const en of sub.entries) addEdge(pe, en, "default");
      }
      prevExits = sub.exits;
    }
    return { entries: seqEntries || [], exits: prevExits || [] };
  }

  let body = flow.nodes;
  let endLabel = "Return";
  const last = body[body.length - 1];
  if (last && last.kind === "end") {
    endLabel = last.label;
    body = body.slice(0, -1);
  }
  nodes.push({
    id: endNodeId,
    type: "workflowEnd",
    data: { label: endLabel, nodeKind: "workflow_end" },
  });

  const main = buildSeq(body);
  const bodyFirstEntry = main.entries[0] || endNodeId;
  for (const e of edges) {
    if (e.type === "loop" && e.target === "__BODY_FIRST__") e.target = bodyFirstEntry;
  }
  for (const en of main.entries) addEdge("start", en, "default");
  for (const ex of main.exits) addEdge(ex, endNodeId, "default");

  for (const e of edges) {
    if (hookIds.has(e.target) && !e.label) e.label = "hook";
  }

  return { nodes, edges };
}
