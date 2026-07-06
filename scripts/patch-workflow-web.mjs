// scripts/patch-workflow-web.mjs
//
// Stamps DylanClaw branding into the upstream @workflow/web compiled
// bundles in node_modules. We used to use patch-package, but the upstream
// publishes Vite bundles with content-hashed filenames, and any churn in
// a fresh-install baseline (locally vs. Vercel) made the diff fail to
// apply. This script targets only what we actually need to change:
//
//   1. The Logo() function in the React Router bundles — swapped for an
//      original DylanClaw SVG mark + wordmark.
//   2. The hard-coded `useworkflow.dev/docs` doc links — rewritten to
//      point at our own /docs page.
//
// Runs as a Vercel "build" step (see package.json). Idempotent: re-runs
// on already-patched bundles are no-ops.

import fs from "node:fs";
import path from "node:path";
import { execSync } from "node:child_process";

const WEB_PKG = path.resolve("node_modules/@workflow/web");
const BUILD = path.join(WEB_PKG, "build");

// Vercel's build cache restores prior-deploy node_modules. If a previous
// deploy mutated @workflow/web (e.g. an old patch-package run), the
// cached copy is no longer a canonical upstream baseline and our targeted
// regex replacements miss. Force a clean reinstall of just this one
// package so we always patch against pristine published bytes.
function reinstallClean() {
  try {
    fs.rmSync(WEB_PKG, { recursive: true, force: true });
    execSync(
      "npm install @workflow/web --silent --no-save --no-audit --no-fund --ignore-scripts",
      { stdio: "inherit" }
    );
  } catch (e) {
    console.log("[patch-workflow-web] reinstall failed:", e?.message ?? e);
  }
}

const NEW_DOCS_HOST = "agentos-claw.vercel.app/docs";

// Replacement body for the `Logo({ className } = {})` function in the
// compiled bundles. Uses jsxRuntimeExports which is in scope at the
// definition site in both client and server bundles.
const NEW_LOGO_BODY = `function Logo({ className } = {}) {
  return /* @__PURE__ */ jsxRuntimeExports.jsxs(
    "svg",
    {
      viewBox: "0 0 1100 240",
      fill: "none",
      xmlns: "http://www.w3.org/2000/svg",
      className: className || "h-6 w-auto",
      role: "img",
      "aria-label": "DylanClaw Logo",
      children: [
        /* @__PURE__ */ jsxRuntimeExports.jsx("path", {
          fillRule: "evenodd",
          clipRule: "evenodd",
          d: "M27.5 0H212.5C227.69 0 240 12.31 240 27.5V212.5C240 227.69 227.69 240 212.5 240H27.5C12.31 240 0 227.69 0 212.5V27.5C0 12.31 12.31 0 27.5 0ZM72 60V180H125.36C158.84 180 182 156.84 182 123.36V116.64C182 83.16 158.84 60 125.36 60H72ZM105 90H123.5C141.45 90 152 100.55 152 118.5V121.5C152 139.45 141.45 150 123.5 150H105V90Z",
          fill: "currentColor"
        }),
        /* @__PURE__ */ jsxRuntimeExports.jsx("text", {
          x: "280",
          y: "172",
          fill: "currentColor",
          fontFamily: "Geist, Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
          fontSize: "148",
          fontWeight: "700",
          letterSpacing: "-5",
          children: "DylanClaw"
        })
      ]
    }
  );
}`;

const LOGO_RE = /function Logo\(\{[^}]*\}\s*=\s*\{\}\)\s*\{/;

function patchLogo(file) {
  let s = fs.readFileSync(file, "utf8");
  if (s.includes("DylanClaw Logo")) return "skip-already";
  const m = LOGO_RE.exec(s);
  if (!m) return "no-logo";
  let depth = 1;
  let i = m.index + m[0].length;
  while (i < s.length && depth > 0) {
    const c = s.charCodeAt(i);
    if (c === 123) depth++;
    else if (c === 125) depth--;
    i++;
  }
  if (depth !== 0) return "brace-walk-failed";
  const out = s.slice(0, m.index) + NEW_LOGO_BODY + s.slice(i);
  fs.writeFileSync(file, out);
  return "patched";
}

function patchDocs(file) {
  let s = fs.readFileSync(file, "utf8");
  if (!s.includes("useworkflow.dev/docs")) return "no-docs";
  const out = s.replaceAll("useworkflow.dev/docs", NEW_DOCS_HOST);
  if (out === s) return "no-change";
  fs.writeFileSync(file, out);
  return "patched";
}

// Upstream's home.tsx gates the "Workflows" tab behind an isLocalBackend
// check — it only shows when serverConfig.backendId is the local-fs
// world. On a Vercel deployment the production backend (`@workflow/
// world-vercel`) trips that gate to false and the tab disappears, even
// though the manifest is reachable via our outputFileTracingIncludes +
// WORKFLOW_MANIFEST_PATH wiring. Force the gate open so all three
// (Runs / Hooks / Workflows) render on prod.
function patchWorkflowsTabGate(file) {
  let s = fs.readFileSync(file, "utf8");
  const needle =
    `isLocalBackend = serverConfig.backendId === "local" || serverConfig.backendId === "@workflow/world-local"`;
  if (!s.includes(needle)) return "no-gate";
  const out = s.replace(needle, `isLocalBackend = true`);
  if (out === s) return "no-change";
  fs.writeFileSync(file, out);
  return "patched";
}

// Adds a "Live Steps" tab — runtime execution view that aggregates
// step events from currently-running workflow runs. Polls every 5s.
// Each row is clickable; click navigates to /run/<runId> where the
// upstream run-detail view shows the step in its workflow context.
//
// The view derives from two existing RPC functions in scope:
//   - fetchRuns$1(env, { sortOrder, limit, status }) → { data: [...runs] }
//   - fetchEvents$1(env, runId, { sortOrder, limit, withData }) → { data: [...events] }
//
// Step events fan out as: step_created / step_started / step_completed /
// step_failed / step_retrying. We group by correlationId, take the most
// recent state per (run, step), and surface only the non-terminal ones
// in the "live" feed.
function patchAddLiveStepsTab(file) {
  let s = fs.readFileSync(file, "utf8");
  if (s.includes("function LiveStepsList(")) return "skip-already";
  if (!s.includes("function WorkflowsList(")) return "no-anchor";
  // Requires the static Steps tab patch to have run first (we splice in
  // after the steps TabsTrigger / TabsContent that patch creates).
  if (!s.includes(`children: "Steps"`)) return "no-steps-anchor";

  // The RPC client function name differs between bundles:
  //   - client (home-D6JmZfUp.js) re-exports them as `fetchRuns` / `fetchEvents`
  //   - server (server-build-JwomDvSn.js) has them as `fetchRuns$1` / `fetchEvents$1`
  // Detect what's actually in this bundle and template the right one in.
  const fetchRunsName = s.includes("fetchRuns$1(env") ? "fetchRuns$1" : "fetchRuns";
  const fetchEventsName = s.includes("fetchEvents$1(env") ? "fetchEvents$1" : "fetchEvents";

  const liveStepsList = `function LiveStepsList() {
  const navigate = useNavigate();
  const env = reactExports.useMemo(function () { return {}; }, []);
  const [lastRefreshTime, setLastRefreshTime] = reactExports.useState(function () { return new Date(); });
  const [rows, setRows] = reactExports.useState([]);
  const [loading, setLoading] = reactExports.useState(true);
  const [error, setError] = reactExports.useState(null);
  const [currentPage, setCurrentPage] = reactExports.useState(0);
  const PAGE_SIZE = 25;
  const fetchData = reactExports.useCallback(async function () {
    try {
      setError(null);
      // unwrapServerActionResult returns { result, error } where result
      // is the createResponse payload (for fetchRuns that's
      // { data, cursor, hasMore }).
      // Fetch the most recent runs without filtering by status — some
      // backends don't honor the status filter the same way, and the
      // user has already told us they expect to see in-flight work, so
      // we'd rather over-fetch and post-filter per-step than miss
      // recently-started runs that haven't propagated to the "running"
      // index yet.
      const runsUnwrap = await unwrapServerActionResult(__FETCH_RUNS__(env, {
        sortOrder: "desc",
        limit: 30
      }));
      if (runsUnwrap && runsUnwrap.error) {
        setError(runsUnwrap.error);
        return;
      }
      const runs = (runsUnwrap && runsUnwrap.result && runsUnwrap.result.data) || [];
      const limited = runs.slice(0, 20);
      const groups = await Promise.all(limited.map(async function (run) {
        try {
          const eventsUnwrap = await unwrapServerActionResult(__FETCH_EVENTS__(env, run.runId, {
            sortOrder: "asc",
            limit: 500,
            withData: true
          }));
          if (eventsUnwrap && eventsUnwrap.error) return [];
          const events = (eventsUnwrap && eventsUnwrap.result && eventsUnwrap.result.data) || [];
          const byId = new Map();
          for (const e of events) {
            const t = (e && e.eventType) || "";
            if (!t.startsWith("step_")) continue;
            const key = e.correlationId || (e.eventData && e.eventData.stepId) || e.eventId || JSON.stringify(e).slice(0, 32);
            const stepName = (e.eventData && (e.eventData.stepName || e.eventData.name)) || (e.eventData && e.eventData.stepId) || "(step)";
            const ts = (e.createdAt || e.eventData && e.eventData.createdAt) || null;
            const prev = byId.get(key);
            if (!prev) {
              byId.set(key, {
                key: run.runId + ":" + key,
                runId: run.runId,
                workflowName: run.workflowName,
                stepName: String(stepName),
                eventType: t,
                createdAt: ts,
                lastEventAt: ts
              });
            } else {
              prev.eventType = t;
              prev.lastEventAt = ts;
            }
          }
          return Array.from(byId.values());
        } catch (_e) {
          return [];
        }
      }));
      const flat = [];
      for (const g of groups) for (const r of g) {
        // Surface both live and historical step states — completed and
        // failed steps stay in the table (marked with their terminal
        // state) so the user sees a Runs-tab-style chronology of step
        // activity instead of a list that empties out the moment the
        // workflow finishes.
        flat.push(r);
      }
      flat.sort(function (a, b) {
        const ta = a.lastEventAt || "";
        const tb = b.lastEventAt || "";
        return tb < ta ? -1 : tb > ta ? 1 : 0;
      });
      setRows(flat);
      setLastRefreshTime(new Date());
    } catch (e) {
      setError(e);
    } finally {
      setLoading(false);
    }
  }, [env]);
  reactExports.useEffect(function () {
    fetchData();
    const id = setInterval(fetchData, 5000);
    return function () { clearInterval(id); };
  }, [fetchData]);
  const toolbar = jsxRuntimeExports.jsxs("div", {
    className: "flex items-end justify-between gap-2 mb-4",
    children: [
      jsxRuntimeExports.jsxs("div", {
        className: "flex items-end gap-2",
        children: [
          jsxRuntimeExports.jsx("p", { className: "text-sm text-muted-foreground", children: "Last refreshed" }),
          jsxRuntimeExports.jsx(RelativeTime, {
            date: lastRefreshTime,
            className: "text-sm text-muted-foreground",
            type: "distance"
          })
        ]
      }),
      jsxRuntimeExports.jsxs(Button, {
        variant: "outline",
        size: "sm",
        onClick: fetchData,
        disabled: loading && rows.length === 0,
        children: ["Refresh"]
      })
    ]
  });
  if (loading && rows.length === 0) {
    return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
      toolbar,
      jsxRuntimeExports.jsx(TableSkeleton, { variant: "workflows", rows: 6 })
    ] });
  }
  if (error) {
    return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
      toolbar,
      jsxRuntimeExports.jsxs(Alert, { variant: "destructive", children: [
        jsxRuntimeExports.jsx(CircleAlert, { className: "h-4 w-4" }),
        jsxRuntimeExports.jsx(AlertTitle, { children: "Error Loading Live Steps" }),
        jsxRuntimeExports.jsx(AlertDescription, { children: (error && error.message) || String(error) })
      ] })
    ] });
  }
  if (rows.length === 0) {
    return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
      toolbar,
      jsxRuntimeExports.jsx(Card, { children: jsxRuntimeExports.jsxs(CardContent, { className: "p-12 text-center", children: [
        jsxRuntimeExports.jsx(Workflow, { className: "mx-auto h-12 w-12 text-muted-foreground mb-4" }),
        jsxRuntimeExports.jsx("h3", { className: "text-lg font-semibold mb-2", children: "No Live Steps" }),
        jsxRuntimeExports.jsx("p", { className: "text-sm text-muted-foreground", children: "No in-flight steps right now. Dispatch a workflow run to see them appear here." })
      ] }) })
    ] });
  }
  const totalPages = Math.max(1, Math.ceil(rows.length / PAGE_SIZE));
  const safePage = Math.min(currentPage, totalPages - 1);
  const start = safePage * PAGE_SIZE;
  const visible = rows.slice(start, start + PAGE_SIZE);
  const showingFrom = rows.length === 0 ? 0 : start + 1;
  const showingTo = Math.min(rows.length, start + PAGE_SIZE);
  const pagination = jsxRuntimeExports.jsxs("div", {
    className: "flex items-center justify-between mt-4",
    children: [
      jsxRuntimeExports.jsx("div", {
        className: "text-sm text-muted-foreground",
        children: showingFrom + "-" + showingTo + " of " + rows.length
      }),
      jsxRuntimeExports.jsxs("div", { className: "flex gap-2 items-center", children: [
        jsxRuntimeExports.jsxs(Button, {
          variant: "outline",
          size: "sm",
          onClick: function () { setCurrentPage(function (p) { return Math.max(0, p - 1); }); },
          disabled: safePage === 0,
          children: ["Previous"]
        }),
        jsxRuntimeExports.jsxs(Button, {
          variant: "outline",
          size: "sm",
          onClick: function () { setCurrentPage(function (p) { return Math.min(totalPages - 1, p + 1); }); },
          disabled: safePage >= totalPages - 1,
          children: ["Next"]
        })
      ] })
    ]
  });
  // Map step event types to the canonical workflow-run statuses
  // (pending / running / completed / failed) that StatusBadge expects.
  function stepEventToStatus(eventType) {
    if (eventType === "step_completed") return "completed";
    if (eventType === "step_failed") return "failed";
    if (eventType === "step_started" || eventType === "step_retrying") return "running";
    return "pending";
  }
  return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
    toolbar,
    jsxRuntimeExports.jsx(Card, {
      className: "overflow-hidden bg-background",
      children: jsxRuntimeExports.jsx(CardContent, {
        className: "p-0",
        children: jsxRuntimeExports.jsxs(Table, { children: [
          jsxRuntimeExports.jsx(TableHeader, { children: jsxRuntimeExports.jsxs(TableRow, { children: [
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "Step" }),
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "Workflow" }),
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "Run" }),
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "State" }),
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "Last event" })
          ] }) }),
          jsxRuntimeExports.jsx(TableBody, { children: visible.map(function (row) {
            const parsedName = parseWorkflowName(row.workflowName || "");
            const wfShort = (parsedName && parsedName.shortName) || row.workflowName || "?";
            const status = stepEventToStatus(row.eventType);
            return jsxRuntimeExports.jsxs(TableRow, {
              className: "cursor-pointer group relative",
              onClick: function () { navigate("/run/" + row.runId); },
              children: [
                jsxRuntimeExports.jsx(TableCell, {
                  className: "py-2",
                  children: jsxRuntimeExports.jsx(CopyableText, {
                    text: row.stepName, overlay: true, children: row.stepName
                  })
                }),
                jsxRuntimeExports.jsx(TableCell, {
                  className: "py-2",
                  children: jsxRuntimeExports.jsx(CopyableText, {
                    text: row.workflowName || "", overlay: true, children: wfShort
                  })
                }),
                jsxRuntimeExports.jsx(TableCell, {
                  className: "font-mono text-xs py-2",
                  children: jsxRuntimeExports.jsx(CopyableText, {
                    text: row.runId || "", overlay: true, children: row.runId || ""
                  })
                }),
                jsxRuntimeExports.jsx(TableCell, {
                  className: "py-2",
                  children: jsxRuntimeExports.jsx(StatusBadge, { status: status })
                }),
                jsxRuntimeExports.jsx(TableCell, {
                  className: "py-2 text-muted-foreground text-xs",
                  children: row.lastEventAt
                    ? jsxRuntimeExports.jsx(RelativeTime, { date: row.lastEventAt })
                    : "-"
                })
              ]
            }, row.key);
          }) })
        ] })
      })
    }),
    pagination
  ] });
}
`;
  const liveStepsListResolved = liveStepsList
    .replace(/__FETCH_RUNS__/g, fetchRunsName)
    .replace(/__FETCH_EVENTS__/g, fetchEventsName);
  s = s.replace("function WorkflowsList(", liveStepsListResolved + "function WorkflowsList(");

  // Add a TabsTrigger value="live-steps" right after the steps trigger.
  const triggerSteps =
    `isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsTrigger, {\n          value: "steps",\n          children: "Steps"\n        })`;
  if (!s.includes(triggerSteps)) return "no-trigger-anchor";
  s = s.replace(
    triggerSteps,
    triggerSteps +
      `, isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsTrigger, {\n          value: "live-steps",\n          children: "Live Steps"\n        })`
  );

  // Add a TabsContent value="live-steps" after the steps TabsContent.
  // Locate the start of the steps TabsContent (which patchAddStepsTab
  // wrote) and brace-walk through it.
  const contentStartNeedle =
    `isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsContent, {\n        value: "steps",`;
  const startIdx = s.indexOf(contentStartNeedle);
  if (startIdx < 0) return "no-content-anchor";
  const openParen = s.indexOf("(", s.indexOf("jsxRuntimeExports.jsx", startIdx));
  if (openParen < 0) return "no-paren";
  let depth = 1;
  let i = openParen + 1;
  while (i < s.length && depth > 0) {
    const c = s.charCodeAt(i);
    if (c === 40) depth++;
    else if (c === 41) depth--;
    else if (c === 34) {
      i++;
      while (i < s.length && (s.charCodeAt(i) !== 34 || s.charCodeAt(i - 1) === 92)) i++;
    }
    i++;
  }
  if (depth !== 0) return "brace-walk-failed";
  const insertion =
    `, isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsContent, {\n        value: "live-steps",\n        children: /* @__PURE__ */ jsxRuntimeExports.jsx(LiveStepsList, {})\n      })`;
  s = s.slice(0, i) + insertion + s.slice(i);

  fs.writeFileSync(file, s);
  return "patched";
}

// Polish the graph canvas:
//   1. Animated dashed edges for parallel/loop/conditional. The
//      existing code sets `strokeDasharray` on those edge types but
//      leaves `animated = false`, so the dashes are static. Flipping
//      `animated = true` makes them flow visually, signalling the
//      "this is a control-flow edge, not a sequential step transition"
//      distinction at a glance.
//   2. Background variant: replace the default dots Background with a
//      lines variant configured to be just as visually subtle. The
//      gap matches the upstream's defaults; the color uses the same
//      theme token the rest of the dashboard already pulls (`--border`)
//      so it adapts to light/dark mode without a hardcoded value.
function patchCanvasPolish(file) {
  let s = fs.readFileSync(file, "utf8");
  // No global sentinel early-return: each sub-edit (animation flip,
  // background swap, viewport anchor, prior-viewport repatch) has its
  // own `s.includes(replace)` guard. The global sentinel used to block
  // viewport-tuning re-runs against already-polished bundles, leaving
  // stale `defaultViewport` values in place after we bumped the patch.
  let changed = false;

  // The convertToReactFlowEdges helper declares `const animated = false`
  // and re-derives it per-edge. We need to reassign inside each case, so
  // flip the declaration to `let`. Without this the runtime throws
  // "Assignment to constant variable" the moment a control-flow edge
  // hits the animation patch below.
  if (s.includes("const animated = false") && !s.includes("let animated = false")) {
    s = s.replace("const animated = false", "let animated = false");
    changed = true;
  }

  // Animate dashed edges. Inject `animated = true;` inside each of the
  // three switch cases that already set strokeDasharray. The anchor is
  // the unique `strokeDasharray = "X,Y"` line for that case.
  const animationPairs = [
    ['strokeDasharray = "4,4";\n        edgeType = "smoothstep";',
     'strokeDasharray = "4,4";\n        edgeType = "smoothstep";\n        animated = true; /* __patched_canvas_polish */'],
    ['strokeDasharray = "8,4";\n        edgeType = "step";',
     'strokeDasharray = "8,4";\n        edgeType = "step";\n        animated = true;'],
    ['strokeDasharray = "8,4";\n        edgeType = "smoothstep";\n        isConditional = true;',
     'strokeDasharray = "8,4";\n        edgeType = "smoothstep";\n        isConditional = true;\n        animated = true;'],
  ];
  for (const [needle, replace] of animationPairs) {
    if (s.includes(needle) && !s.includes(replace)) {
      s = s.replace(needle, replace);
      changed = true;
    }
  }

  // Switch the canvas Background to lines with a subtle theme-aware
  // color. The default Background renders dots (BackgroundVariant.Dots
  // in upstream's enum). We swap to the `lines` variant string —
  // react-flow accepts the string directly.
  const bgNeedle = "jsxRuntimeExports.jsx(Background, {})";
  const bgReplace =
    'jsxRuntimeExports.jsx(Background, { variant: "lines", gap: 128, lineWidth: 0.5, color: "var(--border)" })';
  if (s.includes(bgNeedle) && !s.includes(bgReplace)) {
    s = s.replaceAll(bgNeedle, bgReplace);
    changed = true;
  }

  // Auto-fit the canvas to the actual node bounding box once the nodes
  // have DOM dimensions. Trying to anchor with a hardcoded
  // defaultViewport never worked reliably because the world-coordinate
  // bounding box of the graph depends on how many layers, how wide the
  // fan-out is, and how the layout algorithm spaces them — values that
  // change per workflow. The article the user referenced calls this
  // out: React Flow has no built-in layout, and even with a layout
  // algorithm you still need the camera to anchor to whatever bounds
  // the algorithm produced.
  //
  // Solution: turn fitView back on AND inject an onInit that re-fits
  // after the nodes have been measured (the first auto-fit fires
  // before the DOM measurement pass, so it gets the bounds wrong).
  // We re-fit in three phases:
  //   1. instance.fitView() inside onInit (covers the synchronous case)
  //   2. requestAnimationFrame → re-fit (catches the measurement pass)
  //   3. setTimeout 200ms → re-fit with a short transition (catches
  //      late layout settle from font load / async measurement)
  // fitViewOptions pads ~15% around the bounding box and caps zoom at
  // 1.1 so even a one-node workflow doesn't blow up to fill the canvas.
  const fitViewOptionsLiteral =
    "{ padding: 0.15, maxZoom: 1.1, minZoom: 0.25, duration: 0 }";
  const onInitLiteral =
    "(__rfInstance) => { try { __rfInstance.fitView(" +
    fitViewOptionsLiteral +
    "); requestAnimationFrame(() => __rfInstance.fitView(" +
    fitViewOptionsLiteral +
    ")); setTimeout(() => __rfInstance.fitView({ padding: 0.15, maxZoom: 1.1, minZoom: 0.25, duration: 200 }), 220); } catch {} }";

  const viewportReplace =
    `fitView: true,\n      fitViewOptions: ${fitViewOptionsLiteral},\n      onInit: ${onInitLiteral},\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: 0, y: 0, zoom: 1 },`;

  // Pristine upstream (fitView: true + defaultViewport at origin)
  const upstreamNeedle =
    `fitView: true,\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: 0, y: 0, zoom: 1 },`;
  if (s.includes(upstreamNeedle) && !s.includes("__rfInstance")) {
    s = s.replaceAll(upstreamNeedle, viewportReplace);
    changed = true;
  }
  // Repatch every prior viewport we wrote before this rewrite so stale
  // node_modules from previous dev runs self-correct.
  const priorViewportPatterns = [
    `fitView: false,\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: -140, y: 20, zoom: 0.85 },`,
    `fitView: false,\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: 150, y: 250, zoom: 0.85 },`,
    `fitView: false,\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: 200, y: 540, zoom: 0.85 },`,
    `fitView: false,\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: 500, y: 540, zoom: 0.85 },`,
    `fitView: false,\n      minZoom: 0.1,\n      maxZoom: 2,\n      defaultViewport: { x: 200, y: 200, zoom: 0.85 },`,
  ];
  for (const prior of priorViewportPatterns) {
    if (s.includes(prior) && !s.includes("__rfInstance")) {
      s = s.replaceAll(prior, viewportReplace);
      changed = true;
    }
  }

  if (!changed) return "no-anchor";
  fs.writeFileSync(file, s);
  return "patched";
}

// Flip the workflow graph layout from top-to-bottom to left-to-right.
// Two coordinated edits:
//   1. calculateNodePositions: swap x and y assignment so layers spread
//      horizontally (one column per layer) and siblings within a layer
//      spread vertically.
//   2. reactFlowNodes.push sites: add `sourcePosition: "right"` and
//      `targetPosition: "left"` so the DefaultNode renderer puts its
//      handles on the sides instead of top/bottom. Without this, the
//      nodes are visually left-to-right but the edges still try to
//      attach to the top/bottom handles and curve awkwardly.
function patchGraphLR(file) {
  let s = fs.readFileSync(file, "utf8");
  // Two independent sentinels because the two edits land in different
  // files: the position formula only exists in server-build + home,
  // while the push sites also exist in workflow-graph-viewer. Sharing
  // one sentinel would cause idempotent double-patching on files that
  // only get one of the two edits.
  const haveLayoutSentinel = s.includes("/* __patched_lr_layout */");
  const havePushSentinel = s.includes('sourcePosition: "right", targetPosition: "left"');
  if (haveLayoutSentinel && havePushSentinel) return "skip-already";

  let changed = false;

  // ---- (1) calculateNodePositions: swap x/y formula ----
  // Position assignment uses two spellings across bundles. We patch
  // both the client (node2) and server (node2) variants in one shot,
  // looking for the distinctive `startX + indexInLayer * LAYOUT.HORIZONTAL_SPACING`
  // line and replacing the position object atomically.
  const positionNeedle =
    `x: startX + indexInLayer * LAYOUT.HORIZONTAL_SPACING,\n        y: LAYOUT.START_Y + layer * LAYOUT.VERTICAL_SPACING`;
  const positionReplace =
    `x: LAYOUT.START_X + layer * LAYOUT.HORIZONTAL_SPACING /* __patched_lr_layout */,\n        y: LAYOUT.START_Y + (indexInLayer - (layerNodes.length - 1) / 2) * LAYOUT.VERTICAL_SPACING`;
  if (!haveLayoutSentinel && s.includes(positionNeedle)) {
    s = s.replace(positionNeedle, positionReplace);
    changed = true;
  }

  // ---- (2) reactFlowNodes.push: add source/target handle positions ----
  // There are three push sites: loop, conditional, and default. All
  // share the `expandParent: true,` line as a stable anchor near the
  // top of the object literal. Adding the handle positions there
  // means every node type picks up the LR routing.
  const pushNeedle = `expandParent: true,`;
  const pushReplace = `expandParent: true, sourcePosition: "right", targetPosition: "left",`;
  if (!havePushSentinel && s.includes(pushNeedle)) {
    s = s.replaceAll(pushNeedle, pushReplace);
    changed = true;
  }

  if (!changed) return "no-anchor";
  fs.writeFileSync(file, s);
  return "patched";
}

// Preserve the `steps` field through adaptManifest. The upstream
// adaptManifest() returns `{ version, workflows }` only — the manifest's
// `steps` map (which our Steps tab depends on) gets dropped on the way
// to React state. Patch the two return sites to also forward `steps`.
function patchAdaptManifestKeepsSteps(file) {
  let s = fs.readFileSync(file, "utf8");
  if (!s.includes("function adaptManifest(")) return "no-anchor";
  if (s.includes("/* __patched_keep_steps */")) return "skip-already";

  let changed = false;
  // The bundler names the adaptManifest parameter differently across
  // bundles (`raw` in the client home chunk, `raw2` in the server
  // build), so we patch both spellings without relying on knowing
  // which one applies to which file.
  for (const v of ["raw", "raw2"]) {
    const lateNeedle = `return {
    version: ${v}.version,
    workflows
  };`;
    const lateReplace = `return {
    version: ${v}.version,
    workflows,
    steps: (${v} && ${v}.steps) || {} /* __patched_keep_steps */
  };`;
    if (s.includes(lateNeedle)) {
      s = s.replace(lateNeedle, lateReplace);
      changed = true;
    }

    const earlyNeedle =
      `return { version: (${v} == null ? void 0 : ${v}.version) || "1.0.0", workflows: {} };`;
    const earlyReplace =
      `return { version: (${v} == null ? void 0 : ${v}.version) || "1.0.0", workflows: {}, steps: (${v} && ${v}.steps) || {} };`;
    if (s.includes(earlyNeedle)) {
      s = s.replace(earlyNeedle, earlyReplace);
      changed = true;
    }
  }

  if (!changed) return "no-change";
  fs.writeFileSync(file, s);
  return "patched";
}

// Adds a "Steps" tab next to the Workflows tab. The upstream dashboard
// doesn't ship one because steps are normally inspected per-run, but
// our project gets value out of a catalog view (every "use step"
// function registered, grouped by file). Same isLocalBackend gate as
// the Workflows tab so it inherits the force-on we already do.
//
// Three injections per home.tsx bundle (client + server):
//   1. A `StepsList` function definition is hoisted just before
//      WorkflowsList so it shares scope (uses the same
//      useWorkflowGraphManifest hook + jsxRuntimeExports/reactExports
//      globals).
//   2. A TabsTrigger value="steps" is added after the workflows trigger.
//   3. A TabsContent value="steps" wrapping <StepsList/> is added after
//      the workflows content.
function patchAddStepsTab(file) {
  let s = fs.readFileSync(file, "utf8");
  if (s.includes("function StepsList(")) return "skip-already";
  if (!s.includes("function WorkflowsList(")) return "no-anchor";

  // ---- 1. Hoist a StepsList component definition before WorkflowsList ----
  // Renders a Card-wrapped Table that mirrors the Workflows tab's
  // exact typography (TableCell className="py-2", <span className="font-medium">
  // for the prominent name, <code className="text-xs text-muted-foreground">
  // for the file path), plus a toolbar above the table with a
  // "Last refreshed" relative timestamp + a Refresh button — same
  // pattern as the Runs tab's toolbar.
  //
  // Identifiers used here (Card, Table*, Button, RelativeTime,
  // TableSkeleton, Alert, AlertTitle, AlertDescription, CircleAlert,
  // Workflow, Badge, GitBranch, RefreshCw if available, etc.) are all in
  // scope because WorkflowsList — defined right after — uses them.
  const stepsList = `function StepsList() {
  const {
    manifest: graphManifest,
    loading: stepsLoading,
    error: stepsError,
    refetch: refetchManifest
  } = useWorkflowGraphManifest();
  const [lastRefreshTime, setLastRefreshTime] = reactExports.useState(function () { return new Date(); });
  const [currentPage, setCurrentPage] = reactExports.useState(0);
  const PAGE_SIZE = 25;
  const allSteps = reactExports.useMemo(function () {
    const out = [];
    const filesObj = (graphManifest && graphManifest.steps) || {};
    for (const [filePath, stepsObj] of Object.entries(filesObj)) {
      for (const [stepName, entry] of Object.entries(stepsObj || {})) {
        out.push({
          stepName: stepName,
          filePath: filePath,
          stepId: (entry && entry.stepId) || stepName
        });
      }
    }
    return out.sort(function (a, b) {
      return a.stepName.localeCompare(b.stepName);
    });
  }, [graphManifest]);
  reactExports.useEffect(function () {
    if (!stepsLoading) setLastRefreshTime(new Date());
  }, [stepsLoading, graphManifest]);
  const handleRefresh = function () {
    if (typeof refetchManifest === "function") refetchManifest();
    else setLastRefreshTime(new Date());
  };
  const toolbar = jsxRuntimeExports.jsxs("div", {
    className: "flex items-end justify-between gap-2 mb-4",
    children: [
      jsxRuntimeExports.jsxs("div", {
        className: "flex items-end gap-2",
        children: [
          jsxRuntimeExports.jsx("p", { className: "text-sm text-muted-foreground", children: "Last refreshed" }),
          jsxRuntimeExports.jsx(RelativeTime, {
            date: lastRefreshTime,
            className: "text-sm text-muted-foreground",
            type: "distance"
          })
        ]
      }),
      jsxRuntimeExports.jsxs(Button, {
        variant: "outline",
        size: "sm",
        onClick: handleRefresh,
        disabled: stepsLoading,
        children: ["Refresh"]
      })
    ]
  });
  if (stepsLoading && !graphManifest) {
    return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
      toolbar,
      jsxRuntimeExports.jsx(TableSkeleton, { variant: "workflows", rows: 8 })
    ] });
  }
  if (stepsError) {
    return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
      toolbar,
      jsxRuntimeExports.jsxs(Alert, { variant: "destructive", children: [
        jsxRuntimeExports.jsx(CircleAlert, { className: "h-4 w-4" }),
        jsxRuntimeExports.jsx(AlertTitle, { children: "Error Loading Steps" }),
        jsxRuntimeExports.jsx(AlertDescription, { children: stepsError.message })
      ] })
    ] });
  }
  if (allSteps.length === 0) {
    return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
      toolbar,
      jsxRuntimeExports.jsx(Card, { children: jsxRuntimeExports.jsxs(CardContent, { className: "p-12 text-center", children: [
        jsxRuntimeExports.jsx(Workflow, { className: "mx-auto h-12 w-12 text-muted-foreground mb-4" }),
        jsxRuntimeExports.jsx("h3", { className: "text-lg font-semibold mb-2", children: "No Steps Found" }),
        jsxRuntimeExports.jsx("p", { className: "text-sm text-muted-foreground", children: "No \\"use step\\" functions were found in the graph manifest." })
      ] }) })
    ] });
  }
  const totalPages = Math.max(1, Math.ceil(allSteps.length / PAGE_SIZE));
  const safePage = Math.min(currentPage, totalPages - 1);
  const start = safePage * PAGE_SIZE;
  const visible = allSteps.slice(start, start + PAGE_SIZE);
  const showingFrom = allSteps.length === 0 ? 0 : start + 1;
  const showingTo = Math.min(allSteps.length, start + PAGE_SIZE);
  const pagination = jsxRuntimeExports.jsxs("div", {
    className: "flex items-center justify-between mt-4",
    children: [
      jsxRuntimeExports.jsx("div", {
        className: "text-sm text-muted-foreground",
        children: showingFrom + "-" + showingTo + " of " + allSteps.length
      }),
      jsxRuntimeExports.jsxs("div", { className: "flex gap-2 items-center", children: [
        jsxRuntimeExports.jsxs(Button, {
          variant: "outline",
          size: "sm",
          onClick: function () { setCurrentPage(function (p) { return Math.max(0, p - 1); }); },
          disabled: safePage === 0,
          children: ["Previous"]
        }),
        jsxRuntimeExports.jsxs(Button, {
          variant: "outline",
          size: "sm",
          onClick: function () { setCurrentPage(function (p) { return Math.min(totalPages - 1, p + 1); }); },
          disabled: safePage >= totalPages - 1,
          children: ["Next"]
        })
      ] })
    ]
  });
  return jsxRuntimeExports.jsxs(jsxRuntimeExports.Fragment, { children: [
    toolbar,
    jsxRuntimeExports.jsx(Card, {
      className: "overflow-hidden bg-background",
      children: jsxRuntimeExports.jsx(CardContent, {
        className: "p-0",
        children: jsxRuntimeExports.jsxs(Table, { children: [
          jsxRuntimeExports.jsx(TableHeader, { children: jsxRuntimeExports.jsxs(TableRow, { children: [
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "Step" }),
            jsxRuntimeExports.jsx(TableHead, { className: "bg-background border-b shadow-sm h-10", children: "File" })
          ] }) }),
          jsxRuntimeExports.jsx(TableBody, { children: visible.map(function (step) {
            return jsxRuntimeExports.jsxs(TableRow, {
              children: [
                jsxRuntimeExports.jsx(TableCell, {
                  className: "py-2",
                  children: jsxRuntimeExports.jsx("span", { className: "font-medium", children: step.stepName })
                }),
                jsxRuntimeExports.jsx(TableCell, {
                  className: "py-2",
                  children: jsxRuntimeExports.jsx("code", { className: "text-xs text-muted-foreground", children: step.filePath })
                })
              ]
            }, step.stepId);
          }) })
        ] })
      })
    }),
    pagination
  ] });
}
`;
  s = s.replace("function WorkflowsList(", stepsList + "function WorkflowsList(");

  // ---- 2. Add a TabsTrigger value="steps" next to the workflows one ----
  const triggerWorkflows =
    `isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsTrigger, {\n          value: "workflows",\n          children: "Workflows"\n        })`;
  if (!s.includes(triggerWorkflows)) return "no-trigger-anchor";
  s = s.replace(
    triggerWorkflows,
    triggerWorkflows +
      `, isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsTrigger, {\n          value: "steps",\n          children: "Steps"\n        })`
  );

  // ---- 3. Add a TabsContent value="steps" after the workflows TabsContent ----
  // Brace-walk from the start of the workflows TabsContent call to find
  // its outer jsxRuntimeExports.jsx(...) closing paren, then splice in
  // our steps TabsContent right after that.
  const contentStartNeedle =
    `isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsContent, {\n        value: "workflows",`;
  const startIdx = s.indexOf(contentStartNeedle);
  if (startIdx < 0) return "no-content-anchor";
  // Find the opening `(` of jsxRuntimeExports.jsx(TabsContent, {...
  const openParen = s.indexOf("(", s.indexOf("jsxRuntimeExports.jsx", startIdx));
  if (openParen < 0) return "no-paren";
  let depth = 1;
  let i = openParen + 1;
  while (i < s.length && depth > 0) {
    const c = s.charCodeAt(i);
    if (c === 40) depth++;        // (
    else if (c === 41) depth--;   // )
    else if (c === 34) {          // " — skip JSON-ish strings
      i++;
      while (i < s.length && (s.charCodeAt(i) !== 34 || s.charCodeAt(i - 1) === 92)) i++;
    }
    i++;
  }
  if (depth !== 0) return "brace-walk-failed";
  // i is now one past the closing `)` of the workflows TabsContent jsx call.
  const insertion =
    `, isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsContent, {\n        value: "steps",\n        children: /* @__PURE__ */ jsxRuntimeExports.jsx(StepsList, {})\n      })`;
  s = s.slice(0, i) + insertion + s.slice(i);

  fs.writeFileSync(file, s);
  return "patched";
}

// Patch the upstream's fetchWorkflowsManifest to read DIRECTLY from
// the absolute path our build ships the file at, instead of relying on
// the dashboard's path-search logic.  We verified via the debug endpoint
// that /var/task/app/.well-known/workflow/v1/manifest.json exists on the
// function and parses to 6 workflows — but the dashboard's own path
// search keeps returning empty, which means SOMETHING in its prelude
// (ensureLocalWorldDataDirEnv, getObservabilityCwd, etc.) is short-
// circuiting before the WORKFLOW_MANIFEST_PATH check fires.  Bypass the
// whole thing with a direct read.
function patchManifestReader(file) {
  let s = fs.readFileSync(file, "utf8");
  const needle = `async function fetchWorkflowsManifest(_worldEnv) {`;
  if (!s.includes(needle)) return "no-reader";
  if (s.includes("__patched_manifest_reader")) return "skip-already";
  // We need to splice an early-return BEFORE the existing body. We don't
  // know what `createResponse` is named in this minified bundle locally,
  // so we use the same pattern the existing body uses at its tail.
  const inject =
    `async function fetchWorkflowsManifest(_worldEnv) {\n` +
    `  /* __patched_manifest_reader */\n` +
    `  let __merged = null;\n` +
    `  try {\n` +
    `    const candidates = [\n` +
    `      process.env.WORKFLOW_MANIFEST_PATH,\n` +
    `      "/var/task/app/.well-known/workflow/v1/manifest.json",\n` +
    `      "/var/task/public/.well-known/workflow/v1/manifest.json",\n` +
    `    ].filter(Boolean);\n` +
    `    for (const p of candidates) {\n` +
    `      try {\n` +
    `        const txt = await fs$1.readFile(p, "utf-8");\n` +
    `        const m = JSON.parse(txt);\n` +
    `        if (m && Object.keys(m.workflows || {}).length > 0) { __merged = m; break; }\n` +
    `      } catch (_e) {}\n` +
    `    }\n` +
    `  } catch (_e) {}\n` +
    `  if (__merged) {\n` +
    `    /* Dynamically merge live workforce diagrams (Redis) into the manifest\n` +
    `       object so user-created teams appear without a rebuild/enrich pass. */\n` +
    `    try {\n` +
    `      const __base =\n` +
    `        process.env.WORKFORCE_MANIFEST_URL ||\n` +
    `        (process.env.VERCEL_URL ? "https://" + process.env.VERCEL_URL : "");\n` +
    `      if (__base) {\n` +
    `        const __r = await fetch(__base + "/api/claw?op=workforce_manifest", { cache: "no-store" });\n` +
    `        if (__r && __r.ok) {\n` +
    `          const __frag = await __r.json();\n` +
    `          const __wf = __frag && __frag.workflows;\n` +
    `          if (__wf) {\n` +
    `            __merged.workflows = __merged.workflows || {};\n` +
    `            for (const __k of Object.keys(__wf)) {\n` +
    `              if (__wf[__k] && Object.keys(__wf[__k]).length > 0) __merged.workflows[__k] = __wf[__k];\n` +
    `            }\n` +
    `          }\n` +
    `        }\n` +
    `      }\n` +
    `    } catch (_e) {}\n` +
    `    return createResponse(__merged);\n` +
    `  }\n` +
    `  /* fall through to original body */\n`;
  const out = s.replace(needle, inject);
  if (out === s) return "no-change";
  fs.writeFileSync(file, out);
  return "patched";
}

// Adds our app-native tabs (Agents / Evals / Activity / Logs) to the embedded
// dashboard. Each is an iframe pointing at the corresponding /ui/* page in
// embed mode (?embed=1 strips that page's own chrome), so the rich ReactFlow
// canvas + eval graph render without re-implementing them inside this bundle.
//
// Three injections (same shape as patchAddStepsTab):
//   1. An `AppTabFrame({ src })` component hoisted before WorkflowsList.
//   2. Four TabsTriggers after the Live Steps trigger.
//   3. Four TabsContents after the Live Steps TabsContent.
//
// Gated on isLocalBackend like the other custom tabs so they inherit the
// force-on we already do (patchWorkflowsTabGate sets isLocalBackend = true).
const APP_TABS = [
  { value: "agents", label: "Agents", src: "/ui/agents?embed=1" },
  { value: "evals", label: "Evals", src: "/ui/agent-evals?embed=1" },
  { value: "activity", label: "Activity", src: "/ui/activity?embed=1" },
  { value: "logs", label: "Logs", src: "/ui/logs?embed=1" },
];

function patchAddAppTabs(file) {
  let s = fs.readFileSync(file, "utf8");
  if (s.includes("function AppTabFrame(")) return "skip-already";
  if (!s.includes("function WorkflowsList(")) return "no-anchor";
  // Requires the Live Steps tab patch to have run (we splice after its
  // trigger / content).
  if (!s.includes(`children: "Live Steps"`)) return "no-live-anchor";

  // ---- 1. Hoist the iframe wrapper component ----
  const frame = `function AppTabFrame({ src }) {
  return /* @__PURE__ */ jsxRuntimeExports.jsx("div", {
    className: "rounded-lg border bg-background overflow-hidden",
    style: { height: "calc(100vh - 220px)", minHeight: 520 },
    children: /* @__PURE__ */ jsxRuntimeExports.jsx("iframe", {
      src: src,
      title: src,
      style: { width: "100%", height: "100%", border: "0", display: "block" }
    })
  });
}
`;
  s = s.replace("function WorkflowsList(", frame + "function WorkflowsList(");

  // ---- 2. Add TabsTriggers after the Live Steps trigger ----
  const liveTrigger =
    `isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsTrigger, {\n          value: "live-steps",\n          children: "Live Steps"\n        })`;
  if (!s.includes(liveTrigger)) return "no-trigger-anchor";
  const triggerInsert = APP_TABS.map(
    (t) =>
      `, isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsTrigger, {\n          value: "${t.value}",\n          children: "${t.label}"\n        })`
  ).join("");
  s = s.replace(liveTrigger, liveTrigger + triggerInsert);

  // ---- 3. Add TabsContents after the Live Steps TabsContent ----
  const contentStartNeedle =
    `isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsContent, {\n        value: "live-steps",`;
  const startIdx = s.indexOf(contentStartNeedle);
  if (startIdx < 0) return "no-content-anchor";
  const openParen = s.indexOf("(", s.indexOf("jsxRuntimeExports.jsx", startIdx));
  if (openParen < 0) return "no-paren";
  let depth = 1;
  let i = openParen + 1;
  while (i < s.length && depth > 0) {
    const c = s.charCodeAt(i);
    if (c === 40) depth++;
    else if (c === 41) depth--;
    else if (c === 34) {
      i++;
      while (i < s.length && (s.charCodeAt(i) !== 34 || s.charCodeAt(i - 1) === 92)) i++;
    }
    i++;
  }
  if (depth !== 0) return "brace-walk-failed";
  const contentInsert = APP_TABS.map(
    (t) =>
      `, isLocalBackend && /* @__PURE__ */ jsxRuntimeExports.jsx(TabsContent, {\n        value: "${t.value}",\n        children: /* @__PURE__ */ jsxRuntimeExports.jsx(AppTabFrame, { src: "${t.src}" })\n      })`
  ).join("");
  s = s.slice(0, i) + contentInsert + s.slice(i);

  fs.writeFileSync(file, s);
  return "patched";
}

function collectBundleFiles() {
  const dirs = [
    path.join(BUILD, "client/assets"),
    path.join(BUILD, "server/assets"),
  ];
  return dirs.flatMap((d) =>
    fs.existsSync(d)
      ? fs
          .readdirSync(d)
          .filter((f) => f.endsWith(".js"))
          .map((f) => path.join(d, f))
      : []
  );
}

function main() {
  // Force a clean reinstall unconditionally when invoked with `--reinstall`
  // (we wire this on the postinstall hook). The build hook runs without the
  // flag so it just re-verifies/no-ops on already-patched bytes. This dance
  // exists because Vercel's build cache can restore a mutated node_modules
  // from a prior deploy, and we want pristine upstream bytes every time.
  const wantReinstall = process.argv.includes("--reinstall");
  if (wantReinstall) reinstallClean();
  if (!fs.existsSync(BUILD)) {
    console.log("[patch-workflow-web] @workflow/web not installed — skipping");
    return;
  }
  const files = collectBundleFiles();
  let logos = 0;
  let docs = 0;
  let gates = 0;
  for (const f of files) {
    const l = patchLogo(f);
    if (l === "patched") {
      logos++;
      console.log(`[patch-workflow-web] logo  -> ${path.relative(WEB_PKG, f)}`);
    }
    const d = patchDocs(f);
    if (d === "patched") {
      docs++;
      console.log(`[patch-workflow-web] docs  -> ${path.relative(WEB_PKG, f)}`);
    }
    const g = patchWorkflowsTabGate(f);
    if (g === "patched") {
      gates++;
      console.log(`[patch-workflow-web] gate  -> ${path.relative(WEB_PKG, f)}`);
    }
    const r = patchManifestReader(f);
    if (r === "patched") {
      console.log(`[patch-workflow-web] mfst  -> ${path.relative(WEB_PKG, f)}`);
    }
    const t = patchAddStepsTab(f);
    if (t === "patched") {
      console.log(`[patch-workflow-web] steps -> ${path.relative(WEB_PKG, f)}`);
    } else if (t && t !== "skip-already" && t !== "no-anchor") {
      console.log(`[patch-workflow-web] steps SKIP (${t}) ${path.relative(WEB_PKG, f)}`);
    }
    const a = patchAdaptManifestKeepsSteps(f);
    if (a === "patched") {
      console.log(`[patch-workflow-web] adapt -> ${path.relative(WEB_PKG, f)}`);
    }
    const lv = patchAddLiveStepsTab(f);
    if (lv === "patched") {
      console.log(`[patch-workflow-web] live  -> ${path.relative(WEB_PKG, f)}`);
    } else if (lv && lv !== "skip-already" && lv !== "no-anchor" && lv !== "no-steps-anchor") {
      console.log(`[patch-workflow-web] live SKIP (${lv}) ${path.relative(WEB_PKG, f)}`);
    }
    const at = patchAddAppTabs(f);
    if (at === "patched") {
      console.log(`[patch-workflow-web] apptabs -> ${path.relative(WEB_PKG, f)}`);
    } else if (at && at !== "skip-already" && at !== "no-anchor" && at !== "no-live-anchor") {
      console.log(`[patch-workflow-web] apptabs SKIP (${at}) ${path.relative(WEB_PKG, f)}`);
    }
    const lr = patchGraphLR(f);
    if (lr === "patched") {
      console.log(`[patch-workflow-web] lr    -> ${path.relative(WEB_PKG, f)}`);
    }
    const cp = patchCanvasPolish(f);
    if (cp === "patched") {
      console.log(`[patch-workflow-web] cnvs  -> ${path.relative(WEB_PKG, f)}`);
    }
  }
  console.log(
    `[patch-workflow-web] done — logos:${logos} docs:${docs} gates:${gates} files-scanned:${files.length}`
  );
}

main();
