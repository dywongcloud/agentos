// app/lib/openaiBatch.ts
//
// Thin wrapper over the OpenAI Batch API (https://platform.openai.com/docs/guides/batch).
// Batch trades latency for a 50% discount: you upload a JSONL file of requests
// (each line a `{custom_id, method, url, body}`), create a batch over it, poll
// until it completes (async, up to 24h — usually far faster), then download the
// output file (one JSONL line per custom_id) plus an optional error file.
//
// We talk to api.openai.com directly with OPENAI_API_KEY rather than going
// through the AI SDK / Vercel gateway, because Batch is a file+job protocol the
// SDK doesn't model. Only used for the evals/model-compare LLM-judge path
// (app/lib/evals/batchCompare.ts) where async grading is fine — never for
// interactive or agentic turns.

import { env } from "@/app/lib/env";

const OPENAI_BASE = "https://api.openai.com/v1";

export type BatchEndpoint =
  | "/v1/chat/completions"
  | "/v1/responses"
  | "/v1/embeddings";

export type BatchRequest = {
  custom_id: string;
  method: "POST";
  url: BatchEndpoint;
  body: Record<string, unknown>;
};

export type BatchStatus = {
  id: string;
  status:
    | "validating"
    | "in_progress"
    | "finalizing"
    | "completed"
    | "failed"
    | "expired"
    | "cancelling"
    | "cancelled";
  output_file_id: string | null;
  error_file_id: string | null;
  request_counts?: { total: number; completed: number; failed: number };
};

function apiKey(): string {
  const k = env("OPENAI_API_KEY");
  if (!k) throw new Error("OPENAI_API_KEY is not set — required for the Batch API");
  return k;
}

async function openaiFetch(path: string, init: RequestInit): Promise<Response> {
  const res = await fetch(`${OPENAI_BASE}${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${apiKey()}`,
      ...(init.headers ?? {}),
    },
  });
  return res;
}

// Upload a JSONL request file and create a batch over it. Returns the batch id.
export async function submitBatch(
  requests: BatchRequest[],
  opts: { endpoint?: BatchEndpoint; completionWindow?: "24h"; metadata?: Record<string, string> } = {}
): Promise<string> {
  if (requests.length === 0) throw new Error("submitBatch: no requests");
  const endpoint = opts.endpoint ?? "/v1/chat/completions";

  // 1) Upload the JSONL as a file with purpose=batch.
  const jsonl = requests.map((r) => JSON.stringify(r)).join("\n");
  const form = new FormData();
  form.append("purpose", "batch");
  form.append(
    "file",
    new Blob([jsonl], { type: "application/jsonl" }),
    `batch-${Date.now()}.jsonl`
  );

  const fileRes = await openaiFetch("/files", { method: "POST", body: form });
  if (!fileRes.ok) {
    throw new Error(`batch file upload failed (${fileRes.status}): ${await fileRes.text()}`);
  }
  const file = (await fileRes.json()) as { id: string };

  // 2) Create the batch job over that file.
  const batchRes = await openaiFetch("/batches", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      input_file_id: file.id,
      endpoint,
      completion_window: opts.completionWindow ?? "24h",
      metadata: opts.metadata,
    }),
  });
  if (!batchRes.ok) {
    throw new Error(`batch create failed (${batchRes.status}): ${await batchRes.text()}`);
  }
  const batch = (await batchRes.json()) as { id: string };
  return batch.id;
}

export async function getBatchStatus(batchId: string): Promise<BatchStatus> {
  const res = await openaiFetch(`/batches/${encodeURIComponent(batchId)}`, { method: "GET" });
  if (!res.ok) {
    throw new Error(`batch status failed (${res.status}): ${await res.text()}`);
  }
  return (await res.json()) as BatchStatus;
}

// Download a JSONL result/error file and index it by custom_id. Each output
// line is `{custom_id, response: {status_code, body}, error}`.
export async function fetchBatchOutputs(
  fileId: string
): Promise<Map<string, { statusCode: number; body: any; error: any }>> {
  const res = await openaiFetch(`/files/${encodeURIComponent(fileId)}/content`, { method: "GET" });
  if (!res.ok) {
    throw new Error(`batch output download failed (${res.status}): ${await res.text()}`);
  }
  const text = await res.text();
  const out = new Map<string, { statusCode: number; body: any; error: any }>();
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const row = JSON.parse(trimmed) as {
        custom_id: string;
        response?: { status_code: number; body: any };
        error?: any;
      };
      out.set(row.custom_id, {
        statusCode: row.response?.status_code ?? 0,
        body: row.response?.body ?? null,
        error: row.error ?? null,
      });
    } catch {
      /* skip malformed line */
    }
  }
  return out;
}

// Pull the assistant message text out of a /v1/chat/completions response body.
export function chatCompletionText(body: any): string {
  return String(body?.choices?.[0]?.message?.content ?? "");
}

export function isTerminalStatus(s: BatchStatus["status"]): boolean {
  return s === "completed" || s === "failed" || s === "expired" || s === "cancelled";
}
