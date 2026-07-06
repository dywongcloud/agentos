// app/steps/persistDeepJobStep.ts
//
// After a job finishes (deep or normal), persist its full output to the
// tenant's VFS at /workspace/deep-jobs/{jobId}/. Hours or days later the
// chat agent can rediscover it by walking the workspace tree or via memory
// recall. This is the durability layer that prevents long jobs from being
// lost when they fall out of chat history.
//
// What gets written:
//   /workspace/deep-jobs/{jobId}/result.md       — the final synthesis text
//   /workspace/deep-jobs/{jobId}/manifest.json   — metadata + file inventory
//   /workspace/deep-jobs/{jobId}/subtasks/N-K.md — each subtask output
//   /workspace/deep-jobs/{jobId}/citations.txt   — all unique source URLs
//
// Plus a long-term memory entry of kind=project so /recall finds it later:
//   title: "Deep job: <first 60 chars of prompt>"
//   summary: meta + file pointer
//   labels: ["deep-job", jobId, modality]
//
// All file writes use the same Redis schema that agentTurn's
// write_virtual_file tool uses (vfs:{tid}:{sid}:paths SET + per-node JSON),
// so existing read tools (read_virtual_file, list_session_assets,
// virtual_shell) discover the files transparently.

import { getStore } from "@/app/lib/store";
import { putMemory } from "@/app/lib/memoryStore";
import { recordActivity } from "@/app/lib/activityLog";
import type { SubtaskResult } from "@/app/machines/jobMachine";
import { getJobMeta } from "@/app/lib/jobStore";

// --- VFS helpers (mirroring agentTurn's schema exactly) --------------------

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

function vfsPathsKey(userId: string, sessionId: string): string {
  return `vfs:${userId}:${sessionId}:paths`;
}

function vfsNodeKey(userId: string, sessionId: string, path: string): string {
  return `vfs:${userId}:${sessionId}:node:${sanitizePath(path)}`;
}

type VfsFileNode = {
  type: "file";
  path: string;
  content: string;
  createdAt: string;
  updatedAt: string;
};

async function vfsWriteFile(args: {
  userId: string;
  sessionId: string;
  path: string;
  content: string;
}): Promise<string> {
  const store = getStore();
  const safe = sanitizePath(args.path);
  const now = new Date().toISOString();
  const existing = await store.get<VfsFileNode>(
    vfsNodeKey(args.userId, args.sessionId, safe)
  );
  const node: VfsFileNode = {
    type: "file",
    path: safe,
    content: args.content,
    createdAt: existing?.createdAt ?? now,
    updatedAt: now,
  };
  await store.set(vfsNodeKey(args.userId, args.sessionId, safe), node);
  await store.sadd(vfsPathsKey(args.userId, args.sessionId), safe);
  return safe;
}

// --- the step --------------------------------------------------------------

export type PersistDeepJobInput = {
  jobId: string;
};

export type PersistDeepJobResult = {
  ok: boolean;
  rootPath: string;
  filesWritten: string[];
  memoryId?: string;
};

function safeFilenamePart(s: string): string {
  return String(s ?? "")
    .replace(/[^a-zA-Z0-9_.-]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 48) || "x";
}

// Mostly idempotent: writing the same files twice just refreshes the
// updatedAt. The memory entry creation is NOT idempotent (each call makes
// a new memory) — caller should only invoke once per job completion.
export async function persistDeepJobStep(
  args: PersistDeepJobInput
): Promise<PersistDeepJobResult> {
  "use step";

  const meta = await getJobMeta(args.jobId);
  if (!meta) {
    return { ok: false, rootPath: "", filesWritten: [] };
  }

  const tenantId = meta.tenantId;
  const sessionId = meta.sessionId;
  const root = `/workspace/deep-jobs/${args.jobId}`;

  // We don't have the SubtaskResult[] in JobMeta directly; the workflow
  // passes the final snapshot context, but the step only has access to
  // jobMeta + Redis. Snapshot persistence (job:{id}:snapshot) holds the
  // xstate context with subtaskResults — read it to enrich the writeup.
  const store = getStore();
  type SnapshotShape = {
    context?: {
      subtaskResults?: SubtaskResult[];
      modality?: string | null;
      assumptions?: string[];
      finalSynthesis?: string | null;
    };
  };
  const snap = await store.get<SnapshotShape>(`job:${args.jobId}:snapshot`);
  const subtaskResults: SubtaskResult[] = snap?.context?.subtaskResults ?? [];
  const modality = snap?.context?.modality ?? null;
  const assumptions = snap?.context?.assumptions ?? [];
  const finalText =
    snap?.context?.finalSynthesis ?? meta.resultText ?? "";

  const filesWritten: string[] = [];

  // 1) result.md — the final synthesis. This is the primary artifact.
  const headerLines = [
    `# Deep job ${args.jobId}`,
    "",
    `**Prompt:** ${meta.prompt}`,
    "",
    `**Kind:** ${meta.kind}    **Status:** ${meta.status}    **Modality:** ${modality ?? "n/a"}`,
    "",
    meta.createdAt
      ? `**Started:** ${new Date(meta.createdAt).toISOString()}`
      : "",
    meta.finishedAt
      ? `**Finished:** ${new Date(meta.finishedAt).toISOString()}`
      : "",
    typeof meta.estimatedCost === "number"
      ? `**Cost (est.):** $${meta.estimatedCost.toFixed(3)}`
      : "",
    assumptions.length
      ? `**Assumptions:**\n${assumptions.map((a) => `  - ${a}`).join("\n")}`
      : "",
    "",
    "---",
    "",
  ].filter(Boolean);
  const resultDoc = headerLines.join("\n") + "\n" + (finalText || "(no text)");
  filesWritten.push(
    await vfsWriteFile({
      userId: tenantId,
      sessionId,
      path: `${root}/result.md`,
      content: resultDoc,
    })
  );

  // 2) subtasks/{i}-{kind}.md — one per subtask
  const allCitations = new Set<string>();
  for (let i = 0; i < subtaskResults.length; i++) {
    const s = subtaskResults[i];
    const idx = String(i + 1).padStart(2, "0");
    const safeKind = safeFilenamePart(s.kind);
    const subBody = [
      `# Subtask ${i + 1} (${s.kind})`,
      "",
      `**Goal:** ${s.goal}`,
      s.citations.length
        ? `\n**Citations:**\n${s.citations.map((c) => `- ${c}`).join("\n")}`
        : "",
      s.artifacts.length
        ? `\n**Artifacts (VFS paths):**\n${s.artifacts.map((a) => `- ${a}`).join("\n")}`
        : "",
      "",
      "---",
      "",
      s.output || "(no output)",
    ]
      .filter(Boolean)
      .join("\n");
    filesWritten.push(
      await vfsWriteFile({
        userId: tenantId,
        sessionId,
        path: `${root}/subtasks/${idx}-${safeKind}.md`,
        content: subBody,
      })
    );
    for (const c of s.citations) {
      if (typeof c === "string" && c.startsWith("http")) allCitations.add(c);
    }
  }

  // 3) citations.txt — every unique source URL across the run
  if (allCitations.size > 0) {
    filesWritten.push(
      await vfsWriteFile({
        userId: tenantId,
        sessionId,
        path: `${root}/citations.txt`,
        content: Array.from(allCitations).join("\n") + "\n",
      })
    );
  }

  // 4) manifest.json — meta + file inventory; lets future reads enumerate
  // the run without globbing.
  const manifest = {
    jobId: args.jobId,
    prompt: meta.prompt,
    kind: meta.kind,
    status: meta.status,
    modality,
    createdAt: meta.createdAt,
    finishedAt: meta.finishedAt,
    estimatedCost: meta.estimatedCost,
    finalArtifacts: meta.resultArtifacts ?? [],
    files: filesWritten,
    subtaskCount: subtaskResults.length,
    citationCount: allCitations.size,
  };
  filesWritten.push(
    await vfsWriteFile({
      userId: tenantId,
      sessionId,
      path: `${root}/manifest.json`,
      content: JSON.stringify(manifest, null, 2) + "\n",
    })
  );

  // 5) long-term memory entry so /recall and the chat agent can rediscover
  // this run by topic-keyword (even when chat history has rolled past it).
  // We skip enrichMemory here to keep the step cost-bounded — write a
  // structured project memory directly.
  let memoryId: string | undefined;
  try {
    const promptHead = meta.prompt.slice(0, 80).replace(/\s+/g, " ").trim();
    const mem = await putMemory({
      tenantId,
      kind: "project",
      title: `Deep job: ${promptHead || args.jobId}`,
      content: finalText || meta.prompt,
      summary:
        `Saved at ${root}/result.md` +
        (modality ? ` (modality: ${modality})` : "") +
        `. ${subtaskResults.length} subtask(s)` +
        (allCitations.size ? `, ${allCitations.size} source(s)` : "") +
        ".",
      labels: [
        "deep-job",
        args.jobId,
        ...(modality ? [modality] : []),
      ],
      importance: 0.7,
      fields: {
        jobId: args.jobId,
        vfsRoot: root,
        resultPath: `${root}/result.md`,
        manifestPath: `${root}/manifest.json`,
        modality,
        finishedAt: meta.finishedAt ?? Date.now(),
      },
    });
    memoryId = mem.id;
  } catch {
    // Memory write failure shouldn't fail the whole persistence.
  }

  await recordActivity(tenantId, {
    kind: "job",
    summary: `persisted job ${args.jobId} → ${root} (${filesWritten.length} files${memoryId ? ", +memory" : ""})`,
    meta: {
      jobId: args.jobId,
      root,
      files: filesWritten.length,
      memoryId: memoryId ?? null,
    },
  });

  return { ok: true, rootPath: root, filesWritten, memoryId };
}
