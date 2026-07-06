// app/api/ui/vfs/list/route.ts
//
// Powers the Files tab on /ui — a Google-Drive-style browser over the
// per-tenant VFS that lives in Redis under `vfs:{userId}:{sessionId}:...`.
//
// The VFS is sharded per chat session (every Telegram/Web/etc. session has
// its own paths SET and node JSONs). For the UI we union all of a user's
// sessions into a single virtual tree, since users think in terms of "my
// files" not "my files in session X". When the same path exists in two
// sessions, the most recently updated node wins.
//
// Response shape:
//   { ok, path, breadcrumbs, dirs[], files[] }
//
//   dirs:  { name, path, childCount, lastModified }
//   files: { name, path, size, mimeType, lastModified, sessionId }
//
// dirs are synthesized — the VFS doesn't store explicit dir nodes for every
// folder, so we infer them from path prefixes of file nodes.

import { NextResponse } from "next/server";
import { Redis } from "@upstash/redis";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { resolveUiTenant } from "@/app/lib/uiTenant";
import { env } from "@/app/lib/env";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

type VfsNode =
  | {
      type: "file";
      path: string;
      content: string;
      createdAt: string;
      updatedAt: string;
    }
  | {
      type: "dir";
      path: string;
      createdAt: string;
      updatedAt: string;
    };

type DirEntry = {
  name: string;
  path: string;
  childCount: number;
  lastModified?: string;
};

type FileEntry = {
  name: string;
  path: string;
  size: number;
  mimeType: string;
  lastModified: string;
  sessionId: string;
};

let cachedRedis: Redis | null = null;
function getRedis(): Redis | null {
  if (cachedRedis) return cachedRedis;
  const url =
    env("KV_REST_API_URL") ?? env("UPSTASH_REDIS_REST_URL") ?? "";
  const token =
    env("KV_REST_API_TOKEN") ?? env("UPSTASH_REDIS_REST_TOKEN") ?? "";
  if (!url || !token) return null;
  cachedRedis = new Redis({ url, token });
  return cachedRedis;
}

function sanitizePath(input: string): string {
  let p = String(input ?? "").trim();
  if (!p) p = "/workspace";
  if (!p.startsWith("/")) p = `/workspace/${p}`;
  p = p.replace(/\/+/g, "/");
  const parts = p.split("/");
  const out: string[] = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (out.length > 0) out.pop();
      continue;
    }
    out.push(part);
  }
  return `/${out.join("/")}`;
}

function basename(p: string): string {
  const i = p.lastIndexOf("/");
  return i >= 0 ? p.slice(i + 1) : p;
}

function inferMime(name: string): string {
  const ext = (name.split(".").pop() ?? "").toLowerCase();
  const m: Record<string, string> = {
    txt: "text/plain",
    md: "text/markdown",
    json: "application/json",
    csv: "text/csv",
    html: "text/html",
    js: "text/javascript",
    ts: "text/plain",
    py: "text/x-python",
    sh: "text/x-sh",
    yml: "text/yaml",
    yaml: "text/yaml",
    xml: "application/xml",
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    webp: "image/webp",
    gif: "image/gif",
    svg: "image/svg+xml",
    pdf: "application/pdf",
    mp3: "audio/mpeg",
    wav: "audio/wav",
    mp4: "video/mp4",
    webm: "video/webm",
    zip: "application/zip",
  };
  return m[ext] ?? "application/octet-stream";
}

// Iterate SCAN across the full keyspace. Upstash's client returns
// `[cursor, keys]` per call; we loop until cursor is "0". Pattern-matching
// happens server-side so the volume of work is bounded by matching keys.
async function scanAll(
  redis: Redis,
  match: string
): Promise<string[]> {
  const out: string[] = [];
  let cursor: string | number = 0;
  let safety = 0;
  do {
    const [next, batch] = (await redis.scan(cursor as any, {
      match,
      count: 200,
    })) as [string, string[]];
    for (const k of batch) out.push(k);
    cursor = next;
    if (++safety > 200) break; // belt-and-suspenders against runaway loops
  } while (String(cursor) !== "0");
  return out;
}

export async function GET(req: Request) {
  await requireUiAuthPage();

  const url = new URL(req.url);
  const userId = await resolveUiTenant(url.searchParams.get("userId"));
  if (!userId) {
    return NextResponse.json({
      ok: true,
      path: "/workspace",
      breadcrumbs: [{ name: "workspace", path: "/workspace" }],
      dirs: [],
      files: [],
      empty: true,
      reason: "no tenant resolved",
    });
  }

  const path = sanitizePath(url.searchParams.get("path") ?? "/workspace");

  const redis = getRedis();
  if (!redis) {
    return NextResponse.json(
      { ok: false, error: "Upstash Redis is not configured" },
      { status: 500 }
    );
  }

  // 1. Discover every session this user has VFS data in by SCANning for
  //    the per-session paths SET key. The pattern intentionally escapes
  //    nothing — userId is e.g. "telegram:123456789" which Upstash's SCAN
  //    treats as literal characters.
  const pathsKeys = await scanAll(redis, `vfs:${userId}:*:paths`);

  // 2. For each session, pull its full paths SET in parallel. Caps:
  //    we hard-stop after 50 sessions to bound work for huge tenants.
  const sessionInfos = pathsKeys.slice(0, 50).map((k) => {
    // key: vfs:{userId}:{sessionId}:paths — sessionId may contain ":"
    const stripped = k.slice(`vfs:${userId}:`.length, -":paths".length);
    return { sessionId: stripped, key: k };
  });

  const sessionPaths = await Promise.all(
    sessionInfos.map(async (s) => {
      const members = (await redis.smembers(s.key)) as string[];
      return { sessionId: s.sessionId, paths: members };
    })
  );

  // 3. Build the merged tree:
  //    - prefix = `${path}/` (or just "/" when path is "/")
  //    - any stored path strictly under the prefix contributes to this
  //      level (direct child if no further "/", otherwise a synthetic dir)
  const prefix = path === "/" ? "/" : `${path}/`;
  const dirMap = new Map<string, DirEntry>();
  const fileMap = new Map<string, FileEntry & { mtime: number }>();

  type PendingFile = {
    relative: string;
    fullPath: string;
    sessionId: string;
  };
  const pendingFiles: PendingFile[] = [];

  for (const { sessionId, paths } of sessionPaths) {
    for (const p of paths) {
      if (!p.startsWith(prefix)) continue;
      const remainder = p.slice(prefix.length);
      if (!remainder) continue;
      const slash = remainder.indexOf("/");
      if (slash === -1) {
        // Direct child — either a file at `p` or a dir node we'll learn
        // about later. We need the node JSON to know which.
        pendingFiles.push({
          relative: remainder,
          fullPath: p,
          sessionId,
        });
      } else {
        // Nested — synthesize an intermediate dir entry at `<prefix><name>`.
        const name = remainder.slice(0, slash);
        const dirPath = `${prefix}${name}`.replace(/\/+/g, "/");
        const cur = dirMap.get(dirPath) ?? {
          name,
          path: dirPath,
          childCount: 0,
          lastModified: undefined,
        };
        cur.childCount += 1;
        dirMap.set(dirPath, cur);
      }
    }
  }

  // 4. Hydrate direct-child node JSONs in parallel to learn file vs dir
  //    + grab size / mtime.
  const nodeKeyOf = (sessionId: string, p: string) =>
    `vfs:${userId}:${sessionId}:node:${p}`;
  const nodes = await Promise.all(
    pendingFiles.map((f) =>
      redis.get<VfsNode>(nodeKeyOf(f.sessionId, f.fullPath))
    )
  );

  for (let i = 0; i < pendingFiles.length; i++) {
    const f = pendingFiles[i]!;
    const node = nodes[i];
    if (!node) continue;
    if (node.type === "dir") {
      const cur = dirMap.get(f.fullPath) ?? {
        name: f.relative,
        path: f.fullPath,
        childCount: 0,
        lastModified: undefined,
      };
      cur.lastModified = pickLater(cur.lastModified, node.updatedAt);
      dirMap.set(f.fullPath, cur);
    } else {
      // file — last-write-wins across sessions.
      const size =
        typeof node.content === "string"
          ? new TextEncoder().encode(node.content).length
          : 0;
      const mtime = Date.parse(node.updatedAt || node.createdAt || "") || 0;
      const existing = fileMap.get(f.fullPath);
      if (existing && existing.mtime >= mtime) continue;
      fileMap.set(f.fullPath, {
        name: f.relative,
        path: f.fullPath,
        size,
        mimeType: inferMime(f.relative),
        lastModified: node.updatedAt || node.createdAt || "",
        sessionId: f.sessionId,
        mtime,
      });
    }
  }

  // 5. Backfill lastModified on synthesized dirs by inspecting any one
  //    file inside (best-effort — saves an N+1).
  for (const dir of dirMap.values()) {
    if (dir.lastModified) continue;
    for (const f of fileMap.values()) {
      if (f.path.startsWith(`${dir.path}/`)) {
        dir.lastModified = pickLater(dir.lastModified, f.lastModified);
      }
    }
  }

  const dirs = Array.from(dirMap.values()).sort((a, b) =>
    a.name.localeCompare(b.name)
  );
  const files = Array.from(fileMap.values())
    .sort((a, b) => b.mtime - a.mtime)
    .map(({ mtime: _mtime, ...rest }) => rest);

  const breadcrumbs = (() => {
    const segs = path.split("/").filter(Boolean);
    const out: { name: string; path: string }[] = [];
    let acc = "";
    for (const s of segs) {
      acc += `/${s}`;
      out.push({ name: s, path: acc });
    }
    if (out.length === 0) out.push({ name: "workspace", path: "/workspace" });
    return out;
  })();

  return NextResponse.json({
    ok: true,
    path,
    breadcrumbs,
    dirs,
    files,
    empty: dirs.length === 0 && files.length === 0,
  });
}

function pickLater(a: string | undefined, b: string | undefined): string | undefined {
  if (!a) return b;
  if (!b) return a;
  return Date.parse(a) >= Date.parse(b) ? a : b;
}
