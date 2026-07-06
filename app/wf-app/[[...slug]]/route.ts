// app/wf-app/[[...slug]]/route.ts
//
// Catch-all Next.js route that serves the upstream @workflow/web dashboard
// (vercel/workflow's packages/web) inline at /wf-app/*. The actual upstream
// build is in node_modules/@workflow/web/build/{client,server}; we mount the
// upstream Express app via a small Node↔Web bridge so the literal upstream
// code answers each request.
//
// Mount strategy:
//   - URL prefix /wf-app is stripped before handing the request to upstream
//     (so upstream's React Router sees its own root URLs).
//   - HTML responses get `<base href="/wf-app/">` injected so the SPA's
//     relative asset/RPC URLs resolve under our prefix.
//   - Static client assets (/wf-app/assets/...) are served by Express's
//     built-in static middleware that we wire up just like server.js does.

import path from "node:path";
// eslint-disable-next-line @typescript-eslint/ban-ts-comment
// @ts-ignore — express ships without bundled type defs; we treat it as opaque.
import express from "express";

// Static import of the upstream dashboard package — even though we load its
// build files dynamically below, this line tells Next/Vercel to TRACE the
// package so its files are included in the serverless bundle.
import "@workflow/web/server";

import { expressToFetch } from "@/app/lib/expressBridge";
import { getUiCookie, verifyUiToken } from "@/app/lib/uiAuth";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type ExpressApp = any;

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

let appPromise: Promise<ExpressApp> | null = null;

async function getApp(): Promise<ExpressApp> {
  if (appPromise) return appPromise;
  appPromise = (async () => {
    // Resolve build dir. Try process.cwd() (Vercel funcs run with cwd at the
    // project root with node_modules present) and fall back to createRequire.
    let buildDir = path.join(process.cwd(), "node_modules", "@workflow", "web", "build");
    try {
      const { createRequire } = await import("node:module");
      const req = createRequire(import.meta.url);
      const pkgJson = req.resolve("@workflow/web/package.json");
      if (pkgJson) buildDir = path.join(path.dirname(pkgJson), "build");
    } catch {
      // best-effort; fall back to cwd-based path
    }

    // Dynamic import of the upstream's built React Router server bundle.
    const serverEntry = path.join(buildDir, "server/index.js");
    const { pathToFileURL } = await import("node:url");
    const { app: rrApp } = (await import(
      /* webpackIgnore: true */ pathToFileURL(serverEntry).href
    )) as { app: ExpressApp };

    // Same composition order as the upstream server.js: static assets first,
    // then the React Router server app.
    const server = express();
    server.use(
      "/assets",
      express.static(path.join(buildDir, "client/assets"), {
        immutable: true,
        maxAge: "1y",
      })
    );
    server.use(express.static(path.join(buildDir, "client"), { maxAge: "1h" }));
    server.use(rrApp);
    return server;
  })();
  return appPromise;
}

// Paths the auth gate intentionally lets through: static client assets and
// the favicon. They're just bundled JS/CSS/icons with no run data — gating
// them would also break the login page itself if the redirect ever pointed
// here. Everything else (HTML pages + the RPC/stream/manifest data
// endpoints) requires a valid UI session.
function isPublicAssetPath(p: string): boolean {
  return (
    p.startsWith("/assets/") ||
    p === "/favicon.ico" ||
    p.endsWith(".css") ||
    p.endsWith(".js") ||
    p.endsWith(".woff") ||
    p.endsWith(".woff2")
  );
}

function isDataApiPath(p: string): boolean {
  return (
    p === "/api/rpc" ||
    p.startsWith("/api/stream/") ||
    p === "/__manifest" ||
    p.startsWith("/__manifest") ||
    // React Router 7 loader fetches end in `.data`. Returning HTML
    // redirects for these breaks the SPA's fetcher (it expects a data
    // payload, not a login page) — return 401 so the SPA can surface
    // an auth error cleanly.
    p.endsWith(".data")
  );
}

async function handle(req: Request): Promise<Response> {
  const url = new URL(req.url);

  // Strip our /wf-app prefix; upstream expects to be mounted at root. With
  // next.config rewrites forwarding root paths (/, /run/*, /api/rpc, etc.)
  // into here, the user-facing URL stays at root and the upstream React
  // Router build (which has no basename) matches its routes correctly.
  let rewritten = url.pathname.replace(/^\/wf-app/, "") || "/";
  if (rewritten === "") rewritten = "/";

  // Auth gate. Anyone hitting workflow run data must have a valid UI
  // session — the dashboard exposes job/step/event history that should not
  // be world-readable. Same cookie + token the /ui pages use.
  if (!isPublicAssetPath(rewritten)) {
    const token = await getUiCookie();
    if (!verifyUiToken(token)) {
      if (isDataApiPath(rewritten)) {
        return new Response("Unauthorized", { status: 401 });
      }
      // HTML route → bounce to login. After login, return here.
      const next = encodeURIComponent(url.pathname + url.search);
      return Response.redirect(
        new URL(`/ui/login?next=${next}`, url.origin),
        302
      );
    }
  }

  const app = await getApp();
  const rewriteUrl = rewritten + url.search;
  const resp = await expressToFetch(app, req, rewriteUrl);

  // The upstream's express.static stamps assets with
  // `Cache-Control: public, max-age=31536000, immutable` on the assumption
  // that the filename hash is the only way contents change. When we
  // patch-package the compiled bundles in place (e.g. to swap the logo),
  // the hash stays the same and that immutable directive traps browsers
  // on the old version for up to a year. Override to a short revalidating
  // TTL so future in-place patches reach users without a forced hard-
  // reload.
  if (isPublicAssetPath(rewritten)) {
    const h = new Headers(resp.headers);
    h.set("Cache-Control", "public, max-age=300, must-revalidate");
    return new Response(resp.body, {
      status: resp.status,
      statusText: resp.statusText,
      headers: h,
    });
  }
  return resp;
}

export async function GET(req: Request) {
  return handle(req);
}
export async function POST(req: Request) {
  return handle(req);
}
export async function PUT(req: Request) {
  return handle(req);
}
export async function DELETE(req: Request) {
  return handle(req);
}
export async function PATCH(req: Request) {
  return handle(req);
}
