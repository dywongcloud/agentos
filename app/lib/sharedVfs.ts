// app/lib/sharedVfs.ts
//
// A workforce-shared virtual filesystem. Every agent on a team reads/writes the
// same namespace so they can hand files to each other (drafts, scraped data,
// reports) across stages and runs. Reuses the exact VFS key scheme agentTurn
// uses (`vfs:{tenantId}:{session}`), pinned to a stable per-team session
// (`wf:{workforceId}:shared`) so it persists independently of any single run.

import { getStore } from "@/app/lib/store";

export type SharedVfsFile = {
  type: "file";
  path: string;
  content: string;
  createdAt: string;
  updatedAt: string;
};

function sessionFor(workforceId: string): string {
  return `wf:${workforceId}:shared`;
}
function ns(tenantId: string, workforceId: string): string {
  return `vfs:${tenantId}:${sessionFor(workforceId)}`;
}
function pathsKey(tenantId: string, workforceId: string): string {
  return `${ns(tenantId, workforceId)}:paths`;
}
function nodeKey(tenantId: string, workforceId: string, path: string): string {
  return `${ns(tenantId, workforceId)}:node:${sanitize(path)}`;
}

function sanitize(p: string): string {
  return (p || "")
    .trim()
    .replace(/^\/+/, "")
    .replace(/\.\.+/g, ".")
    .replace(/[^A-Za-z0-9._/\- ]/g, "_")
    .slice(0, 200);
}

export async function sharedVfsWrite(args: {
  tenantId: string;
  workforceId: string;
  path: string;
  content: string;
}): Promise<SharedVfsFile> {
  const store = getStore();
  const p = sanitize(args.path);
  const now = new Date().toISOString();
  const existing = await store.get<SharedVfsFile>(
    nodeKey(args.tenantId, args.workforceId, p)
  );
  const node: SharedVfsFile = {
    type: "file",
    path: p,
    content: args.content,
    createdAt: existing?.createdAt ?? now,
    updatedAt: now,
  };
  await store.set(nodeKey(args.tenantId, args.workforceId, p), node);
  await store.sadd(pathsKey(args.tenantId, args.workforceId), p);
  return node;
}

export async function sharedVfsRead(args: {
  tenantId: string;
  workforceId: string;
  path: string;
}): Promise<SharedVfsFile | null> {
  return getStore().get<SharedVfsFile>(
    nodeKey(args.tenantId, args.workforceId, sanitize(args.path))
  );
}

export async function sharedVfsList(args: {
  tenantId: string;
  workforceId: string;
}): Promise<Array<Pick<SharedVfsFile, "path" | "updatedAt"> & { size: number }>> {
  const store = getStore();
  const paths = await store.smembers(pathsKey(args.tenantId, args.workforceId));
  const nodes = await Promise.all(
    paths.map((p) => store.get<SharedVfsFile>(nodeKey(args.tenantId, args.workforceId, p)))
  );
  return nodes
    .filter((n): n is SharedVfsFile => !!n)
    .map((n) => ({ path: n.path, updatedAt: n.updatedAt, size: n.content.length }))
    .sort((a, b) => (a.path < b.path ? -1 : 1));
}
