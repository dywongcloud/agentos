// scripts/nbriWorkflows.mjs
//
// Builds definition-only WDK graphs for the 4 NBRI business-process
// workflows so they render as real flows in the Workflow DevKit dashboard
// WITHOUT any executable workflow code. The flow content lives in
// app/ui/workflows/nbriFlows.ts (also used by the custom diagram view); we
// extract that NBRI_FLOWS array literal and translate each Flow into the WDK
// { nodes, edges } graph vocabulary reverse-engineered from the real
// workflows:
//   node.type:  workflowStart | primitive | conditional | workflowEnd
//   edge.type:  default | conditional (label=branch) | loop
//   metadata:   loopId | conditionalId | conditionalBranch
//
// `buildNbriWorkflows(root)` returns the object to splice in under
// manifest.workflows["app/workflows/nbri.ts"]. It is invoked from the
// post-build enrich-workflow-manifest.mjs so it survives the manifest
// regeneration that `next build` performs.

import { readFileSync } from "node:fs";
import { join } from "node:path";

const FILE_KEY = "app/workflows/nbri.ts";
const NAME = {
  kickoff: "nbriKickoff",
  predesign: "nbriPredesign",
  design: "nbriDesign",
  deployment: "nbriDeployment",
};

function loadFlows(root) {
  const src = readFileSync(join(root, "app/ui/workflows/nbriFlows.ts"), "utf8");
  const marker = "export const NBRI_FLOWS: Flow[] =";
  const start = src.indexOf(marker);
  if (start < 0) throw new Error("NBRI_FLOWS marker not found");
  // Skip past the `=` so we don't match the `[]` in the `Flow[]` annotation.
  const arrStart = src.indexOf("[", start + marker.length);
  let depth = 0;
  let end = -1;
  for (let i = arrStart; i < src.length; i++) {
    const ch = src[i];
    if (ch === "[") depth++;
    else if (ch === "]") {
      depth--;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  if (end < 0) throw new Error("array literal end not found");
  const literal = src.slice(arrStart, end + 1);
  return new Function(`return (${literal});`)();
}

const A = (s) => (s ? ` [${s.toUpperCase()}]` : "");
const ACT = (s) => (s ? ` · ${s}` : "");

function nodeLabel(n) {
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
      return n.label;
  }
}
function laneLabel(l) {
  return `${l.label}${ACT(l.actor)}${A(l.auto)}`;
}

function buildGraph(flow) {
  const nodes = [];
  const edges = [];
  const hookIds = new Set();
  let nId = 0;
  let cId = 0;
  let lId = 0;
  let eId = 0;
  const newNode = (type, label, nodeKind, metadata) => {
    const id = `node_${nId++}`;
    const node = { id, type, data: { label, nodeKind } };
    if (metadata) node.metadata = metadata;
    nodes.push(node);
    return id;
  };
  const addEdge = (source, target, type, label) => {
    const e = { id: `e_${eId++}`, source, target, type };
    if (label) e.label = label;
    edges.push(e);
  };

  nodes.push({
    id: "start",
    type: "workflowStart",
    data: { label: `Start: ${flow.title}`, nodeKind: "workflow_start" },
  });
  const endNodeId = "end";
  let bodyFirstEntry = null;

  function buildNode(fn) {
    switch (fn.kind) {
      case "trigger":
      case "sleep": {
        const id = newNode("primitive", nodeLabel(fn), "primitive");
        return { entries: [id], exits: [id] };
      }
      case "step": {
        // Real WDK steps render as the green "step" node (and get tallied in
        // the dashboard's step count). The enrich script uses the same
        // type/nodeKind for genuine workflow steps.
        const id = newNode("step", nodeLabel(fn), "step");
        return { entries: [id], exits: [id] };
      }
      case "hook": {
        // Hooks stay WDK "primitive" (the viewer legend is literally
        // "Primitive (sleep, hook)"), which keeps them visually distinct from
        // green step nodes. We also tag them so the edge feeding the hook gets
        // a tiny "hook" label — marking the hook *outside* the node box too.
        const id = newNode("primitive", nodeLabel(fn), "primitive", {
          hook: true,
        });
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
        const exits = [];
        for (const br of fn.branches) {
          const sub = buildSeq(br.nodes, condId, br.label);
          for (const en of sub.entries)
            addEdge(cnode, en, "conditional", br.label);
          exits.push(...sub.exits);
        }
        return { entries: [cnode], exits };
      }
      default:
        throw new Error(`unknown kind ${fn.kind}`);
    }
  }

  function buildSeq(seq, condId, condBranch) {
    let seqEntries = null;
    let prevExits = null;
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
        for (const pe of prevExits)
          for (const en of sub.entries) addEdge(pe, en, "default");
      }
      prevExits = sub.exits;
    }
    return { entries: seqEntries || [], exits: prevExits || [] };
  }

  let body = flow.nodes;
  let endLabel = "Return";
  if (body.length && body[body.length - 1].kind === "end") {
    endLabel = body[body.length - 1].label;
    body = body.slice(0, -1);
  }
  nodes.push({
    id: endNodeId,
    type: "workflowEnd",
    data: { label: endLabel, nodeKind: "workflow_end" },
  });

  const main = buildSeq(body);
  bodyFirstEntry = main.entries[0] || endNodeId;
  for (const e of edges) {
    if (e.type === "loop" && e.target === "__BODY_FIRST__")
      e.target = bodyFirstEntry;
  }
  for (const en of main.entries) addEdge("start", en, "default");
  for (const ex of main.exits) addEdge(ex, endNodeId, "default");

  // Minimal "outside the node" hook marker: label the (unlabeled) edge that
  // feeds each hook node so the hook reads as a hook on the graph, not just
  // via the ⏸ glyph inside the node box.
  for (const e of edges) {
    if (hookIds.has(e.target) && !e.label) e.label = "hook";
  }

  return { nodes, edges };
}

// Returns { "app/workflows/nbri.ts": { nbriKickoff: { workflowId, graph }, ... } }
export function buildNbriWorkflows(root = process.cwd()) {
  const flows = loadFlows(root);
  const out = {};
  for (const flow of flows) {
    const name = NAME[flow.id];
    if (!name) throw new Error(`no manifest name for flow ${flow.id}`);
    out[name] = {
      workflowId: `workflow//./app/workflows/nbri//${name}`,
      graph: buildGraph(flow),
    };
  }
  return { fileKey: FILE_KEY, workflows: out };
}
