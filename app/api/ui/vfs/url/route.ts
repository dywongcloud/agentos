// app/api/ui/vfs/url/route.ts
//
// Mints a short-lived signed URL the UI can hand directly to <a href>
// (or `window.open`) to view/download a VFS file. The actual file serving
// happens at /api/vfs/[...path] which validates the same HMAC signature
// (see app/api/vfs/route.ts). Keeping the signing secret on this server
// route means the browser never sees it.

import { NextResponse } from "next/server";
import crypto from "node:crypto";

import { requireUiAuthPage } from "@/app/lib/uiRequire";
import { env } from "@/app/lib/env";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

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

function safeFilenameSegment(value: string): string {
  const s = String(value ?? "")
    .trim()
    .replace(/[^a-zA-Z0-9._-]+/g, "_")
    .replace(/_+/g, "_")
    .replace(/^_+|_+$/g, "");
  return s || "file";
}

function inferMime(name: string): string {
  const ext = (name.split(".").pop() ?? "").toLowerCase();
  const m: Record<string, string> = {
    txt: "text/plain; charset=utf-8",
    md: "text/markdown; charset=utf-8",
    json: "application/json; charset=utf-8",
    csv: "text/csv; charset=utf-8",
    html: "text/html; charset=utf-8",
    js: "text/javascript; charset=utf-8",
    ts: "text/plain; charset=utf-8",
    py: "text/x-python; charset=utf-8",
    sh: "text/x-sh; charset=utf-8",
    yml: "text/yaml; charset=utf-8",
    yaml: "text/yaml; charset=utf-8",
    xml: "application/xml; charset=utf-8",
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    webp: "image/webp",
    gif: "image/gif",
    svg: "image/svg+xml",
    pdf: "application/pdf",
  };
  return m[ext] ?? "application/octet-stream";
}

export async function GET(req: Request) {
  await requireUiAuthPage();

  const url = new URL(req.url);
  const userId = (url.searchParams.get("userId") ?? "").trim();
  const sessionId = (url.searchParams.get("sessionId") ?? "").trim();
  const path = sanitizePath(url.searchParams.get("path") ?? "");
  const download = url.searchParams.get("download") === "1";

  if (!userId || !sessionId) {
    return NextResponse.json(
      { ok: false, error: "userId and sessionId required" },
      { status: 400 }
    );
  }

  const secret =
    env("VFS_URL_SIGNING_SECRET") ??
    env("ASSET_URL_SIGNING_SECRET") ??
    env("SESSION_ASSET_SIGNING_SECRET") ??
    "";
  if (!secret) {
    return NextResponse.json(
      { ok: false, error: "VFS signing secret not configured" },
      { status: 500 }
    );
  }

  const filename = safeFilenameSegment(basename(path) || "file");
  const mimeType = inferMime(filename);
  const encoding = "utf8" as const;
  const expiresAt = Math.floor(Date.now() / 1000) + 60 * 10;

  const payload = [
    "v1",
    `userId=${userId}`,
    `sessionId=${sessionId}`,
    `path=${path}`,
    `expires=${expiresAt}`,
    `filename=${filename}`,
    `mimeType=${mimeType}`,
    `encoding=${encoding}`,
    `download=${download ? "1" : "0"}`,
  ].join("\n");

  const sig = crypto.createHmac("sha256", secret).update(payload).digest("hex");

  const encodedPath = path
    .split("/")
    .filter(Boolean)
    .map(encodeURIComponent)
    .join("/");

  const qs = new URLSearchParams({
    userId,
    sessionId,
    expires: String(expiresAt),
    filename,
    mimeType,
    encoding,
    download: download ? "1" : "0",
    sig,
  });

  const href = `/api/vfs/${encodedPath}?${qs.toString()}`;
  return NextResponse.json({ ok: true, url: href, expiresAt });
}
