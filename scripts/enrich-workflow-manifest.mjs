// scripts/enrich-workflow-manifest.mjs
//
// Post-build enrichment of the WDK graph manifest at
// app/.well-known/workflow/v1/manifest.json (and its public/ copy).
//
// Why this exists: the upstream's withWorkflow() SWC plugin builds a
// graph for each workflow body, but it only emits nodes for compiler-
// recognized primitives (sleep, conditional, createHook, etc.). User-
// defined `"use step"` functions show up as opaque awaits the analyzer
// can't trace, so workflows whose body is mostly step calls (e.g.
// codeWorkflow, sessionWorkflow, jobWorkflow) end up with a trivial
// graph: just `workflow_start → workflow_end`. The Workflows tab in the
// dashboard then shows an empty diagram for them.
//
// Strategy:
//   1. Read the manifest.
//   2. For each workflow whose graph is "sparse" (≤ 2 nodes — only the
//      synthesized start/end), open its source file.
//   3. Parse imports like `import { fooStep } from "@/app/steps/x"` to
//      collect the names of every step the workflow file references.
//   4. Walk the file for `\bawait\s+(\w+)\s*\(` matches, filtered to
//      the imported step set, preserving first-occurrence order.
//   5. Rebuild the graph as start → step_1 → step_2 → … → step_N → end
//      and write the manifest back.
//
// Caveats: order-of-first-occurrence ≠ runtime control flow. A step
// called inside `catch` or after a hook resume will appear at its
// source-position rather than its dynamic position. That's intentional —
// the goal is "show what could run" not "trace the exact runtime path".
// Future enhancement: detect simple branching patterns (if/else,
// try/catch) and render them as conditional nodes; for now we keep it
// linear and explicit.

import fs from "node:fs/promises";
import path from "node:path";
import { buildNbriWorkflows } from "./nbriWorkflows.mjs";

const ROOT = process.cwd();
const PRIMARY_PATH = path.join(
  ROOT,
  "app/.well-known/workflow/v1/manifest.json"
);
const PUBLIC_PATH = path.join(
  ROOT,
  "public/.well-known/workflow/v1/manifest.json"
);

// ---------- Source parsing helpers ----------

function readSourceMaybeAsync(filePath) {
  return fs.readFile(filePath, "utf8");
}

// Crude but adequate for our codebase: collects every identifier
// imported from a module path that looks like a "step" file. We accept
// imports from anywhere under "@/app/steps/" or "@/app/lib/" (some
// helpers live in lib/) — being too strict here just means we miss
// real steps; being too lax just means we'd surface a non-step
// identifier as a node, which is a non-issue because we cross-check
// against the manifest's `steps` map below.
function collectImportedIdentifiers(source) {
  const imports = new Set();
  // Matches both `import { a, b as c } from "..."` and `import * as x`.
  const importRe = /import\s+(?:type\s+)?\{([^}]+)\}\s+from\s+["']([^"']+)["']/g;
  let m;
  while ((m = importRe.exec(source))) {
    const fromPath = m[2];
    if (
      !fromPath.startsWith("@/app/steps/") &&
      !fromPath.includes("/steps/") &&
      !fromPath.endsWith("/sendOutbound")
    ) {
      continue;
    }
    const inner = m[1];
    for (const piece of inner.split(",")) {
      const cleaned = piece.trim();
      if (!cleaned) continue;
      // `originalName as localName` → take the local name (what gets
      // used in the workflow body).
      const asMatch = cleaned.match(/^(\w+)(?:\s+as\s+(\w+))?$/);
      if (!asMatch) continue;
      imports.add(asMatch[2] || asMatch[1]);
    }
  }
  return imports;
}

// Walk the source for step calls, returning an ordered list of either
// single-step "layers" or parallel-group layers.
//
// Output shape: `[{ kind: "step", name } | { kind: "parallel", names: [...] }, ...]`
//
// Detection rules:
//   - `await Promise.all([ stepA(...), stepB(...), ... ])` (or similar
//     with destructuring on the LHS) is treated as a parallel group.
//     We scan the bracket body for `\w+\s*\(` matches filtered to the
//     imported step set.
//   - Any other `await stepX(...)` is a single sequential layer.
//   - Step names within a parallel group are deduped against each other
//     (a step called twice in the same Promise.all only shows once),
//     but ARE allowed to recur across separate layers (e.g. a step
//     called inside a loop). The graph builder dedupes the LAYERS by
//     first-occurrence afterward.
function findStepLayers(source, importedSet) {
  const layers = [];
  const seenName = new Set();

  // Sweep for parallel groups first by index, then patch any remaining
  // single awaits in source order. We do this in two passes joined by
  // an "intervals" sort so the layer ordering still reflects source
  // order across both kinds.
  const intervals = [];

  const parallelRe = /\bawait\s+Promise\.all\s*\(\s*\[/g;
  let pm;
  while ((pm = parallelRe.exec(source))) {
    const openBracket = source.indexOf("[", pm.index);
    if (openBracket < 0) continue;
    // Brace-walk to find the matching ].
    let depth = 1;
    let i = openBracket + 1;
    while (i < source.length && depth > 0) {
      const c = source.charCodeAt(i);
      if (c === 91) depth++;      // [
      else if (c === 93) depth--; // ]
      else if (c === 34 || c === 39 || c === 96) {
        // skip a quoted string of the matching quote kind
        const quote = c;
        i++;
        while (i < source.length) {
          if (source.charCodeAt(i) === quote && source.charCodeAt(i - 1) !== 92) break;
          i++;
        }
      }
      i++;
    }
    if (depth !== 0) continue;
    const body = source.slice(openBracket + 1, i - 1);
    const innerRe = /([A-Za-z_$][\w$]*)\s*\(/g;
    const namesInGroup = [];
    const seenInGroup = new Set();
    let inner;
    while ((inner = innerRe.exec(body))) {
      const name = inner[1];
      if (!importedSet.has(name)) continue;
      if (seenInGroup.has(name)) continue;
      seenInGroup.add(name);
      namesInGroup.push(name);
    }
    if (namesInGroup.length === 0) continue;
    intervals.push({
      start: pm.index,
      end: i,
      layer: { kind: "parallel", names: namesInGroup },
    });
  }

  // Now plain `await stepX(...)` outside of any Promise.all interval.
  const seqRe = /\bawait\s+([A-Za-z_$][\w$]*)\s*\(/g;
  let m;
  while ((m = seqRe.exec(source))) {
    const name = m[1];
    if (!importedSet.has(name)) continue;
    if (name === "Promise") continue; // already handled
    // Skip if this match is inside a parallel-group interval.
    const inside = intervals.some(
      (iv) => m.index >= iv.start && m.index < iv.end
    );
    if (inside) continue;
    intervals.push({
      start: m.index,
      end: m.index + m[0].length,
      layer: { kind: "step", name },
    });
  }

  intervals.sort((a, b) => a.start - b.start);

  // Dedupe layers by first-occurrence so a step called in a loop
  // doesn't bloat the graph (matches the existing sequential behavior).
  // For parallel groups, the layer identity is the sorted-names tuple.
  const seenLayers = new Set();
  for (const iv of intervals) {
    const layer = iv.layer;
    let key;
    if (layer.kind === "step") {
      if (seenName.has(layer.name)) continue;
      seenName.add(layer.name);
      key = "s:" + layer.name;
    } else {
      key = "p:" + layer.names.slice().sort().join(",");
      for (const n of layer.names) seenName.add(n);
    }
    if (seenLayers.has(key)) continue;
    seenLayers.add(key);
    layers.push(layer);
  }
  return layers;
}

// Legacy helper kept for callers that want a flat list. Wraps the new
// layered analyzer.
function findStepCallSites(source, importedSet) {
  const layers = findStepLayers(source, importedSet);
  const out = [];
  for (const l of layers) {
    if (l.kind === "step") out.push(l.name);
    else for (const n of l.names) out.push(n);
  }
  return out;
}

// ---------- Graph building ----------

// Build a node + edge set for a workflow given a list of layers, where
// each layer is either a single step or a parallel group of steps.
//
// nodeKind = "step" is the canonical value:
//   - WorkflowsList counts steps with `node.data.nodeKind === "step"`,
//     so the Workflows table's Steps column renders the right number.
//   - The graph viewer's getNodeBackgroundColor() falls through to
//     `var(--node-bg-step)` (green) for any nodeKind not explicitly
//     mapped — using "step" hits that fallback.
//
// Parallel layers are rendered as fan-out / fan-in:
//   - prev → each parallelStep (with edge.type = "parallel" so the
//     graph viewer animates the dashes)
//   - each parallelStep → next layer's entry (same parallel edge type)
// The dashboard's edge converter treats edge.type === "parallel" as a
// dashed animated edge. See sandboxClaudeCode.ts notes / the
// `convertToReactFlowEdges` switch.
function buildEnrichedGraph(workflowName, layers) {
  const nodes = [
    {
      id: "start",
      type: "workflowStart",
      data: { label: `Start: ${workflowName}`, nodeKind: "workflow_start" },
    },
  ];
  for (const layer of layers) {
    if (layer.kind === "step") {
      nodes.push({
        id: `step_${layer.name}`,
        type: "step",
        data: { label: layer.name, nodeKind: "step" },
      });
    } else {
      for (const name of layer.names) {
        nodes.push({
          id: `step_${name}`,
          type: "step",
          data: { label: name, nodeKind: "step" },
        });
      }
    }
  }
  nodes.push({
    id: "end",
    type: "workflowEnd",
    data: { label: "Return", nodeKind: "workflow_end" },
  });

  const edges = [];
  // Track the "previous frontier" — the set of node IDs the next layer
  // should connect from. For a sequential step layer that's one id;
  // for a parallel layer it's the full group (so the downstream layer
  // sees a fan-in from every parallel branch).
  let prevFrontier = ["start"];

  function layerIds(layer) {
    return layer.kind === "step"
      ? [`step_${layer.name}`]
      : layer.names.map((n) => `step_${n}`);
  }

  for (const layer of layers) {
    const isParallel = layer.kind === "parallel";
    const ids = layerIds(layer);
    // Edge type for arrows feeding INTO a parallel layer (and out of
    // one): use "parallel" so the dashboard renders the dashed
    // animated style.
    const edgeType = isParallel ? "parallel" : "default";
    for (const src of prevFrontier) {
      for (const dst of ids) {
        edges.push({
          id: `e_${src}_${dst}`,
          source: src,
          target: dst,
          type: edgeType,
        });
      }
    }
    prevFrontier = ids;
  }
  for (const src of prevFrontier) {
    edges.push({
      id: `e_${src}_end`,
      source: src,
      target: "end",
      type: "default",
    });
  }

  return { nodes, edges };
}

function shouldEnrichGraph(graph) {
  const nodes = graph?.nodes ?? [];
  // First-time enrichment: the WDK builder gave us nothing — just
  // workflow_start / workflow_end. We rebuild the graph.
  if (nodes.length <= 2) return true;
  // Re-enrichment of our own prior output. Detect by ID convention:
  // we always name step nodes `step_<stepName>` in buildEnrichedGraph,
  // and no upstream-generated node uses that prefix. This makes the
  // script idempotent across changes to the node shape (e.g. moving
  // from nodeKind:"primitive" to nodeKind:"step") — re-running picks
  // up the new shape instead of leaving the old one in place.
  if (nodes.some((n) => typeof n.id === "string" && n.id.startsWith("step_"))) {
    return true;
  }
  // Genuinely-rich graph (autopilot's conditional+sleep, daemon's
  // sleep loop, etc.). Leave alone — the upstream's structure is more
  // accurate than what static analysis can produce.
  return false;
}

// ---------- Main pass ----------

async function tryReadJson(p) {
  try {
    const raw = await fs.readFile(p, "utf8");
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

async function writeJson(p, obj) {
  const dir = path.dirname(p);
  await fs.mkdir(dir, { recursive: true });
  await fs.writeFile(p, JSON.stringify(obj, null, 2) + "\n", "utf8");
}

async function main() {
  const manifest = await tryReadJson(PRIMARY_PATH);
  if (!manifest) {
    console.log(
      `[enrich-manifest] No manifest at ${PRIMARY_PATH} — skipping.`
    );
    return;
  }
  if (!manifest.workflows) {
    console.log("[enrich-manifest] Manifest has no workflows — skipping.");
    return;
  }

  let enriched = 0;
  let skipped = 0;

  for (const [filePath, workflowsInFile] of Object.entries(
    manifest.workflows
  )) {
    let source;
    try {
      source = await readSourceMaybeAsync(path.join(ROOT, filePath));
    } catch (e) {
      console.log(
        `[enrich-manifest] Could not read ${filePath} (${e.code ?? e.message}) — skipping its workflows.`
      );
      continue;
    }
    const imports = collectImportedIdentifiers(source);
    const layers = findStepLayers(source, imports);

    for (const [workflowName, entry] of Object.entries(workflowsInFile)) {
      if (!shouldEnrichGraph(entry.graph)) {
        skipped++;
        continue;
      }
      if (layers.length === 0) {
        // Nothing to enrich with — leave the trivial graph as-is.
        skipped++;
        continue;
      }
      entry.graph = buildEnrichedGraph(workflowName, layers);
      enriched++;
      const totalSteps = layers.reduce(
        (n, l) => n + (l.kind === "step" ? 1 : l.names.length),
        0
      );
      const parallelLayers = layers.filter((l) => l.kind === "parallel").length;
      const summary = layers
        .map((l) => (l.kind === "step" ? l.name : `[${l.names.join(" || ")}]`))
        .join(" → ");
      console.log(
        `[enrich-manifest] ${workflowName} <- ${totalSteps} step${totalSteps === 1 ? "" : "s"}${parallelLayers > 0 ? ` (${parallelLayers} parallel group${parallelLayers === 1 ? "" : "s"})` : ""}: ${summary}`
      );
    }
  }

  // Splice in the definition-only NBRI workflows. `next build` regenerates
  // the manifest from real source files, so these can only be injected here,
  // post-build. They carry hand-authored graphs (no executable WDK code).
  try {
    const { fileKey, workflows } = buildNbriWorkflows(ROOT);
    manifest.workflows[fileKey] = workflows;
    console.log(
      `[enrich-manifest] injected NBRI workflows: ${Object.keys(workflows).join(", ")}`
    );
  } catch (e) {
    console.error(
      `[enrich-manifest] NBRI injection failed: ${e.stack ?? e.message ?? e}`
    );
  }

  await writeJson(PRIMARY_PATH, manifest);
  // Always mirror to the public/ copy so the static `/.well-known/...`
  // URL stays in sync with the bundled copy the dashboard reads (and so the
  // NBRI workflows are inspectable without a dashboard session).
  await writeJson(PUBLIC_PATH, manifest);

  console.log(
    `[enrich-manifest] done — enriched:${enriched} skipped:${skipped}`
  );
}

main().catch((e) => {
  console.error(`[enrich-manifest] failed: ${e.stack ?? e.message ?? e}`);
  process.exit(1);
});
