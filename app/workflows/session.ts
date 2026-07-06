// app/workflows/session.ts
import type { InboundMessage } from "@/app/lib/normalize";
import type { ModelMessage } from "ai";

import { agentTurn } from "@/app/steps/agentTurn";
import { sendOutbound } from "@/app/steps/sendOutbound";
import {
  loadHistoryStep,
  resolveReplyContextStep,
  saveHistoryStep,
} from "@/app/steps/sessionStateSteps";
import { chatModelNameFor } from "@/app/lib/modelRouting";
import { maybeAutoLearnChatStep } from "@/app/steps/autoLearnStep";
import { isChatStopped } from "@/app/lib/chatControl";

// -----------------------------
// Helpers: multimodal user msg
// -----------------------------
type ImageInput =
  | { kind: "url"; value: string }
  | { kind: "base64"; value: string };

function extractImages(msg: InboundMessage): ImageInput[] {
  const m: any = msg as any;
  const out: ImageInput[] = [];

  // direct fields
  if (typeof m.imageUrl === "string" && m.imageUrl) out.push({ kind: "url", value: m.imageUrl });
  if (typeof m.image_url === "string" && m.image_url) out.push({ kind: "url", value: m.image_url });

  // arrays of urls
  if (Array.isArray(m.imageUrls)) for (const u of m.imageUrls) if (typeof u === "string" && u) out.push({ kind: "url", value: u });
  if (Array.isArray(m.image_urls)) for (const u of m.image_urls) if (typeof u === "string" && u) out.push({ kind: "url", value: u });

  // attachments/media/files
  const arrays: any[][] = [];
  if (Array.isArray(m.attachments)) arrays.push(m.attachments);
  if (Array.isArray(m.media)) arrays.push(m.media);
  if (Array.isArray(m.files)) arrays.push(m.files);

  for (const arr of arrays) {
    for (const a of arr) {
      if (!a) continue;

      const url =
        (typeof a.url === "string" && a.url) ||
        (typeof a.href === "string" && a.href) ||
        (typeof a.downloadUrl === "string" && a.downloadUrl) ||
        (typeof a.download_url === "string" && a.download_url) ||
        "";

      const mime =
        (typeof a.mimeType === "string" && a.mimeType) ||
        (typeof a.mime_type === "string" && a.mime_type) ||
        (typeof a.contentType === "string" && a.contentType) ||
        (typeof a.content_type === "string" && a.content_type) ||
        "";

      const isImageByMime = typeof mime === "string" && mime.startsWith("image/");
      const isImageByExt = typeof url === "string" && /\.(png|jpe?g|webp|gif|bmp|tiff?)($|\?)/i.test(url);

      if (url && (isImageByMime || isImageByExt)) out.push({ kind: "url", value: url });

      const b64 =
        (typeof a.base64 === "string" && a.base64) ||
        (typeof a.data === "string" && a.data) ||
        (typeof a.b64 === "string" && a.b64) ||
        "";

      if (b64 && (isImageByMime || b64.length > 200)) out.push({ kind: "base64", value: b64 });
    }
  }

  // raw base64 fields
  if (typeof m.imageBase64 === "string" && m.imageBase64) out.push({ kind: "base64", value: m.imageBase64 });
  if (typeof m.image_base64 === "string" && m.image_base64) out.push({ kind: "base64", value: m.image_base64 });

  // dedupe
  const seen = new Set<string>();
  return out.filter((x) => {
    const k = `${x.kind}:${x.value}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

function buildUserModelMessage(msg: InboundMessage): ModelMessage {
  const images = extractImages(msg);

  if (!images.length) {
    return { role: "user", content: msg.text ?? "" };
  }

  const parts: any[] = [];
  if (msg.text && msg.text.trim()) parts.push({ type: "text", text: msg.text });

  for (const img of images) {
    if (img.kind === "url") parts.push({ type: "image", image: new URL(img.value) });
    else parts.push({ type: "image", image: img.value });
  }

  return { role: "user", content: parts } as any;
}

function trimHistory(history: ModelMessage[], maxMessages: number): ModelMessage[] {
  const m = Math.max(6, Math.min(200, maxMessages));
  return history.length <= m ? history : history.slice(history.length - m);
}

// -----------------------------
// The workflow (NO HOOKS)
// -----------------------------
export async function sessionWorkflow(sessionId: string, msg: InboundMessage) {
  "use workflow";

  // History load + reply-binding resolve are independent — both feed the
  // user-message composition below but neither depends on the other's
  // output. Run concurrently so chat latency is bounded by max(history,
  // reply) instead of their sum.
  const [historyRaw, replyCtx] = await Promise.all([
    loadHistoryStep(sessionId) as Promise<ModelMessage[]>,
    resolveReplyContextStep({
      sessionId,
      tgMessageId: (msg as any).tgMessageId,
      replyToTgMessageId: (msg as any).replyToTgMessageId,
      text: msg.text ?? "",
    }),
  ]);

  let history = Array.isArray(historyRaw) ? historyRaw : [];

  const max = Number(process.env.HISTORY_MAX_MESSAGES ?? "30");
  history = trimHistory(history, Number.isFinite(max) ? max : 30);

  const { groundingTag } = replyCtx;
  const groundedMsg: InboundMessage = groundingTag
    ? { ...msg, text: `${groundingTag}\n${msg.text ?? ""}` }
    : msg;

  history.push(buildUserModelMessage(groundedMsg));

  // Plain chat (non-/job) runs through the chat model. When TokenHub is
  // enabled this is DeepSeek (TOKENHUB_MODEL, default deepseek-v3.2); otherwise
  // gpt-5.4 (overridable via CHAT_MODEL_NAME). DeepSeek is chat-only — /job and
  // all tool-calling/agentic slots stay on the smart workhorse + modality
  // routing (see app/lib/modelRouting.ts). Complex plain-language agentic
  // messages (multi-app, multi-step phrasing) escalate to Claude Sonnet when
  // ANTHROPIC_API_KEY is configured (Fable is reserved for /deep pro-tier
  // slots); without the key this is chatModelName().
  const chatModel = chatModelNameFor(msg.text);

  // Team-aware tenant: in a bound team group, handleInbound sets msg.tenantId to
  // the shared team namespace so the chat agent's tools (VFS, memory, Composio,
  // automations) all operate on the team's shared state.
  const tenantId = msg.tenantId ?? `${msg.channel}:${msg.senderId}`;

  const result = await agentTurn({
    sessionId,
    userId: tenantId,
    channel: msg.channel,
    history,
    showTyping: msg.channel === "telegram",
    modelName: chatModel,
  });

  history.push({ role: "assistant", content: result.text });
  await saveHistoryStep(sessionId, history);

  // If the user issued /stop while this turn was still running, suppress the
  // final reply — the halt should feel immediate, not "it answered anyway".
  if (await isChatStopped(msg.channel, msg.sessionId)) return;

  // ✅ Avoid duplicates: if Telegram streaming already delivered, do not send again
  if (!(result as any).delivered) {
    await sendOutbound({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: result.text,
    });
  }

  // Passive memory growth: throttled (≤1×/6h/session) distill of durable facts
  // from the chat. A no-op (single Redis read) on virtually every turn; one
  // cheap LLM call only when a window elapses. Reply is already delivered, so
  // this never delays the user. Best-effort.
  try {
    await maybeAutoLearnChatStep({
      tenantId,
      sessionId,
      history,
    });
  } catch {
    // auto-learn must never break the chat turn
  }
}
