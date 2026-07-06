// scripts/genNbriManifest.mjs
//
// Local-preview helper: splices the NBRI definition-only workflows into the
// on-disk manifest WITHOUT a full `next build`. Handy for eyeballing the graph
// in a local dashboard. In CI/prod the same injection happens automatically
// via scripts/enrich-workflow-manifest.mjs (post-build), so this is optional.
//
// Run:  node scripts/genNbriManifest.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { buildNbriWorkflows } from "./nbriWorkflows.mjs";

const ROOT = process.cwd();
const MANIFEST = join(ROOT, "app/.well-known/workflow/v1/manifest.json");

const manifest = JSON.parse(readFileSync(MANIFEST, "utf8"));
const { fileKey, workflows } = buildNbriWorkflows(ROOT);
manifest.workflows[fileKey] = workflows;
for (const [name, wf] of Object.entries(workflows)) {
  console.log(
    `  ${name}: ${wf.graph.nodes.length} nodes, ${wf.graph.edges.length} edges`
  );
}
writeFileSync(MANIFEST, JSON.stringify(manifest, null, 2) + "\n");
console.log(`Patched ${MANIFEST}`);
