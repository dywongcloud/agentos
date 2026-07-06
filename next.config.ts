import { withWorkflow } from "workflow/next";
import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  reactStrictMode: false,
  // @workflow/web's Express app loads at runtime from app/wf-app/[[...slug]]/
  // route.ts — keep it external so Next doesn't try to inline the dashboard
  // build (and so its dynamic asset paths resolve from node_modules at run).
  serverExternalPackages: ["ssh2", "express", "@workflow/web"],
  // Force-include the WDK build manifest in the wf-app function bundle.
  // The dashboard's SSR code reads it via fs.readFile from one of:
  //   - $WORKFLOW_MANIFEST_PATH
  //   - cwd + app/.well-known/workflow/v1/manifest.json
  //   - $WORKFLOW_EMBEDDED_DATA_DIR + manifest.json
  // Vercel's file tracer doesn't pick up .well-known JSON automatically
  // because no JS module imports it, so without this entry the Workflows
  // tab silently renders empty (Runs + Hooks tabs still work because
  // they go through the World, not the manifest).
  outputFileTracingIncludes: {
    "/wf-app/[[...slug]]": [
      "./app/.well-known/workflow/v1/manifest.json",
      "./app/.well-known/workflow/v1/config.json",
    ],
  },
  async rewrites() {
    // The literal upstream @workflow/web dashboard is mounted at root so it
    // doesn't need a basename (the upstream was built to live at /). Each of
    // the upstream's root-owned paths gets forwarded into the wf-app bridge,
    // which strips the /wf-app prefix and dispatches to the upstream Express
    // app. Our own routes (/ui/*, /api/claw, /api/ui/*, /telegram, /webhook,
    // /pair, /sms, /whatsapp, /health, /.well-known/workflow/v1/*) are
    // untouched — no collisions.
    return [
      { source: "/", destination: "/wf-app/" },
      { source: "/run/:path*", destination: "/wf-app/run/:path*" },
      { source: "/api/rpc", destination: "/wf-app/api/rpc" },
      { source: "/api/stream/:path*", destination: "/wf-app/api/stream/:path*" },
      { source: "/__manifest", destination: "/wf-app/__manifest" },
      { source: "/assets/:path*", destination: "/wf-app/assets/:path*" },
      { source: "/favicon.ico", destination: "/wf-app/favicon.ico" },
      // React Router 7 fetches loader data via `.data` URLs whenever the
      // SPA navigates client-side (logo → "/", tab changes, filter changes,
      // pagination, etc.). The index route's loader lives at `/_root.data`;
      // nested routes use `<path>.data` (e.g. `/run/<id>.data`) which the
      // `/run/:path*` rewrite above already catches. Without this entry
      // every in-app navigation to the home view 404s.
      { source: "/_root.data", destination: "/wf-app/_root.data" },
    ];
  },
};

export default withWorkflow(nextConfig);
