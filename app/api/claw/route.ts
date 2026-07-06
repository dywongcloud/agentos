import { NextResponse } from "next/server";
import { start } from "workflow/api";
import { waitUntil } from "@vercel/functions";

import { createReadStream } from "node:fs";
import { writeFile, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { randomUUID } from "node:crypto";
import OpenAI from "openai";

import { sessionWorkflow } from "@/app/workflows/session";
import { daemonWorkflow } from "@/app/workflows/daemon";
import { jobWorkflow } from "@/app/workflows/jobWorkflow";

import {
  createJob,
  getJobMeta,
  getThoughts,
  listActiveJobs,
  listRecentJobs,
  cancelActiveJobs,
} from "@/app/lib/jobStore";
import {
  setChatStopped,
  clearChatStopped,
  isChatStopped,
} from "@/app/lib/chatControl";
import { askJob } from "@/app/lib/jobAsk";
import { classifyDepth } from "@/app/lib/depthClassifier";
import {
  listCookieDomains,
  forgetHostname,
  forgetAll,
} from "@/app/lib/browserAuthStore";
import { runClaudeCode, chooseEngine } from "@/app/lib/sandboxClaudeCode";
import { codeWorkflow } from "@/app/workflows/codeWorkflow";
import {
  createCodeProject,
  getCodeProject,
  getCodeTasks,
  getCodeThoughts,
  listActiveCodeProjects,
  listRecentCodeProjects,
  updateCodeProject,
  type CodeEngine,
} from "@/app/lib/codeProjectStore";
import {
  putMemory,
  deleteMemory,
  listByTag,
  listByKind,
} from "@/app/lib/memoryStore";
import { enrichMemory } from "@/app/lib/memoryEnrichment";
import { memorySummary } from "@/app/lib/memoryRetrieval";
import { recordActivity, listActivity } from "@/app/lib/activityLog";
import { recordAudit } from "@/app/lib/auditLog";
import {
  enableProactive,
  disableProactive,
  isProactiveEnabled,
  getLastProactive,
  getLastProactiveReason,
  cooldownMs,
  isQuietHourNow,
} from "@/app/lib/autopilotProactive";
import { isDebugMode, setDebugMode } from "@/app/lib/debugMode";
import { loadHistoryStep } from "@/app/steps/sessionStateSteps";
import { generateObject } from "ai";
import { textAuxModel } from "@/app/lib/modelRouting";
import { z as zodV4 } from "zod/v4";
import {
  subscribeTrigger,
  unsubscribeTrigger,
  listSubscriptions,
} from "@/app/lib/composioTriggers";
import {
  putAutomation,
  getAutomation,
  listByTenant as listAutomationsByTenant,
  deleteAutomation,
  setEnabled as setAutomationEnabled,
  fireAutomation,
  type Automation,
  type AutomationTrigger,
} from "@/app/lib/automations";
import { compileAutomationStep } from "@/app/steps/compileAutomationStep";
import { compileWorkforceStep } from "@/app/steps/compileWorkforceStep";
import { registerAutomation } from "@/app/lib/registerAutomation";
import { createWorkforceFromSpec } from "@/app/lib/workforceService";
import {
  putSubAgent,
  getSubAgent,
  deleteSubAgent,
  listAgentsByTenant,
  putWorkforce,
  getWorkforce,
  deleteWorkforce,
  listWorkforcesByTenant,
  putAgentBot,
  getAgentBot,
  deleteAgentBot,
  listAgentBotsByTenant,
  scopeForAgent,
  type SubAgent,
  type WorkforceStage,
} from "@/app/lib/agents";
import { agentChatWorkflow } from "@/app/workflows/agentChatWorkflow";
import { launchAgentOptimization } from "@/app/workflows/agentOptimizeWorkflow";
import { dispatchComposioWebhook } from "@/app/lib/composioWebhook";

import type { Channel } from "@/app/lib/identity";
import { makeIdentity } from "@/app/lib/identity";
import {
  getTeam,
  bindGroupChat,
  getTeamByGroupChat,
  getTeamByInviteToken,
  listTeamsForUser,
  createTeam,
  renameTeam,
  removeMember as removeTeamMember,
  deleteTeam,
  teamTenantId,
  teamIdForGroupChat,
  hasUserLeft,
  recordMemberAuto,
  addMember as addTeamMember,
  type Team,
} from "@/app/lib/teams";
import {
  telegramGetMe,
  telegramCreateInviteLink,
} from "@/app/lib/providers/telegram";
import {
  isAddressedToBot,
  shouldChimeIn,
  recordSilentGroupMessage,
} from "@/app/lib/groupGate";
import { safeCompileRegex, safeRegexTest } from "@/app/lib/safeRegex";
import { createPairing, approvePairing, getPendingCode } from "@/app/lib/pairing";
import {
  parsePairCommand,
  normalizeTelegram,
  normalizeTextbeltReply,
  normalizeWhatsApp,
  type InboundMessage,
} from "@/app/lib/normalize";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import {
  telegramValidateWebhook,
  telegramSetMessageReaction,
} from "@/app/lib/providers/telegram";
import { pickReactionEmoji } from "@/app/lib/messageReaction";
import { whatsappVerifyChallenge, verifyWhatsAppSignature } from "@/app/lib/providers/whatsapp";
import {
  getTextbeltApiKeyOptional,
  shouldVerifyTextbeltWebhook,
  verifyTextbeltWebhook,
} from "@/app/lib/providers/textbelt";
import { isInboundAllowed } from "@/app/lib/allowlist";
import { saveSessionMeta, getLastSession, getSessionMeta } from "@/app/lib/sessionMeta";
import { ensurePairingCode, exchangePairingCode, verifyGatewayBearer } from "@/app/lib/gatewayAuth";
import { listTriggerTypes } from "@/app/lib/composioConnections";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// ============================================================
// Utilities
// ============================================================
function jsonOk(extra: any = {}) {
  return NextResponse.json({ ok: true, ...extra });
}

async function handleCronTrigger() {
  const store = getStore();
  const lockKey = "daemon:lock";
  // TTL must be BELOW the cron interval (300s) or the next tick finds the lock
  // still held and skips — stalling the next drain window and custom-trigger
  // polling. The daemon body runs ~285s, so a 290s lock expires just before the
  // next 5-min tick, letting it re-acquire without ever overlapping two daemons.
  const acquired = await store.set(lockKey, String(Date.now()), { exSeconds: 290, nx: true });

  if (acquired) {
    await start(daemonWorkflow, []);
    return jsonOk({ started: true, acquiredLock: true });
  }

  return jsonOk({ started: false, acquiredLock: false });
}

// Explicit slash required: /stop cancels running jobs and /start wipes the
// chat's context, so the bare words must NOT trigger them — "stop" or "start"
// typed mid-conversation (especially in a shared team group) would otherwise
// destructively halt work / erase shared history.
function isStopCmd(text: string) {
  return (text ?? "").trim().toLowerCase() === "/stop";
}

function isStartCmd(text: string) {
  return (text ?? "").trim().toLowerCase() === "/start";
}

// Media proxy allowlist (Bobby CDN only; add more hosts if needed)
const MEDIA_ALLOWED_HOSTS = new Set(["cdn-bobbyapproved.flavcity.com"]);

function safeDecodeMediaUrlParam(raw: string): string {
  // Supports either plain URL-encoded or base64url-encoded.
  try {
    const trimmed = raw.trim();
    if (trimmed.startsWith("http://") || trimmed.startsWith("https://")) return trimmed;

    const b64 = trimmed.replace(/-/g, "+").replace(/_/g, "/");
    const pad = b64.length % 4 === 0 ? "" : "=".repeat(4 - (b64.length % 4));
    return Buffer.from(b64 + pad, "base64").toString("utf8");
  } catch {
    return raw;
  }
}

// ============================================================
// Telegram voice/audio transcription
// ============================================================
let openaiClient: OpenAI | null = null;

function getOpenAIClient() {
  if (!openaiClient) {
    openaiClient = new OpenAI({
      apiKey: env("OPENAI_API_KEY"),
    });
  }

  return openaiClient;
}

function numberEnv(name: string, fallback: number) {
  const raw = process.env[name];
  if (!raw) return fallback;

  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : fallback;
}

const MAX_TELEGRAM_TRANSCRIBE_BYTES = numberEnv(
  "TELEGRAM_TRANSCRIBE_MAX_BYTES",
  20 * 1024 * 1024
);

type TelegramAudioMedia = {
  kind: "voice" | "audio" | "video_note" | "audio_document";
  fileId: string;
  fileName: string;
  mimeType: string;
  duration?: number;
  fileSize?: number;
};

function getTelegramMessage(update: any) {
  return (
    update?.message ??
    update?.edited_message ??
    update?.business_message ??
    update?.channel_post ??
    null
  );
}

function sanitizeFileName(name: string) {
  return name.replace(/[^\w.\-]+/g, "_").slice(0, 160);
}

function extensionFromMime(mimeType: string) {
  const m = mimeType.toLowerCase();

  if (m.includes("ogg") || m.includes("opus")) return ".ogg";
  if (m.includes("mpeg") || m.includes("mp3")) return ".mp3";
  if (m.includes("mp4") || m.includes("m4a")) return ".m4a";
  if (m.includes("wav")) return ".wav";
  if (m.includes("webm")) return ".webm";
  if (m.includes("flac")) return ".flac";

  return ".ogg";
}

function extractTelegramAudioMedia(update: any): TelegramAudioMedia | null {
  const message = getTelegramMessage(update);
  if (!message) return null;

  const messageId = String(message.message_id ?? Date.now());

  if (message.voice?.file_id) {
    return {
      kind: "voice",
      fileId: String(message.voice.file_id),
      fileName: `telegram-voice-${messageId}.ogg`,
      mimeType: String(message.voice.mime_type ?? "audio/ogg"),
      duration: message.voice.duration,
      fileSize: message.voice.file_size,
    };
  }

  if (message.audio?.file_id) {
    const mimeType = String(message.audio.mime_type ?? "audio/mpeg");
    const fileName =
      message.audio.file_name ??
      `telegram-audio-${messageId}${extensionFromMime(mimeType)}`;

    return {
      kind: "audio",
      fileId: String(message.audio.file_id),
      fileName,
      mimeType,
      duration: message.audio.duration,
      fileSize: message.audio.file_size,
    };
  }

  if (message.video_note?.file_id) {
    return {
      kind: "video_note",
      fileId: String(message.video_note.file_id),
      fileName: `telegram-video-note-${messageId}.mp4`,
      mimeType: "video/mp4",
      duration: message.video_note.duration,
      fileSize: message.video_note.file_size,
    };
  }

  const documentMime = String(message.document?.mime_type ?? "");

  if (
    message.document?.file_id &&
    documentMime.toLowerCase().startsWith("audio/")
  ) {
    return {
      kind: "audio_document",
      fileId: String(message.document.file_id),
      fileName:
        message.document.file_name ??
        `telegram-audio-document-${messageId}${extensionFromMime(documentMime)}`,
      mimeType: documentMime,
      fileSize: message.document.file_size,
    };
  }

  return null;
}

async function telegramApi<T>(method: string, body: Record<string, unknown>): Promise<T> {
  const token = env("TELEGRAM_BOT_TOKEN");

  const res = await fetch(`https://api.telegram.org/bot${token}/${method}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });

  const json = await res.json().catch(() => null);

  if (!res.ok || !json?.ok) {
    throw new Error(
      `Telegram API ${method} failed: ${res.status} ${json?.description ?? res.statusText}`
    );
  }

  return json.result as T;
}

async function downloadTelegramFile(fileId: string): Promise<{
  bytes: Buffer;
  filePath: string;
  contentType: string;
}> {
  const token = env("TELEGRAM_BOT_TOKEN");

  const file = await telegramApi<{
    file_id?: string;
    file_unique_id?: string;
    file_size?: number;
    file_path?: string;
  }>("getFile", {
    file_id: fileId,
  });

  if (!file.file_path) {
    throw new Error("Telegram getFile did not return file_path");
  }

  if (file.file_size && file.file_size > MAX_TELEGRAM_TRANSCRIBE_BYTES) {
    throw new Error(`Telegram audio is too large to transcribe: ${file.file_size} bytes`);
  }

  const fileUrl = `https://api.telegram.org/file/bot${token}/${file.file_path}`;
  const res = await fetch(fileUrl, {
    method: "GET",
  });

  if (!res.ok) {
    throw new Error(`Telegram file download failed: ${res.status} ${res.statusText}`);
  }

  const arrayBuffer = await res.arrayBuffer();

  if (arrayBuffer.byteLength > MAX_TELEGRAM_TRANSCRIBE_BYTES) {
    throw new Error(
      `Telegram audio is too large to transcribe: ${arrayBuffer.byteLength} bytes`
    );
  }

  return {
    bytes: Buffer.from(arrayBuffer),
    filePath: file.file_path,
    contentType: res.headers.get("content-type") ?? "application/octet-stream",
  };
}

async function transcribeTelegramAudio(media: TelegramAudioMedia): Promise<string> {
  const downloaded = await downloadTelegramFile(media.fileId);

  const safeName = sanitizeFileName(
    media.fileName || `telegram-audio-${Date.now()}${extensionFromMime(media.mimeType)}`
  );

  const tmpPath = path.join(tmpdir(), `${randomUUID()}-${safeName}`);

  await writeFile(tmpPath, downloaded.bytes);

  try {
    const openai = getOpenAIClient();

    const transcription = await openai.audio.transcriptions.create({
      file: createReadStream(tmpPath),
      model: process.env.TELEGRAM_TRANSCRIBE_MODEL ?? "gpt-4o-mini-transcribe",
    });

    const text =
      typeof transcription === "string"
        ? transcription
        : String((transcription as any)?.text ?? "");

    return text.trim();
  } finally {
    await unlink(tmpPath).catch(() => undefined);
  }
}

function buildTelegramInboundFromUpdate(
  update: any,
  text: string,
  media: TelegramAudioMedia
): InboundMessage | null {
  const message = getTelegramMessage(update);
  if (!message?.chat) return null;

  const chat = message.chat;
  const from = message.from ?? chat;

  // sessionId convention is `telegram:<chatId>` (and `telegram:<chat>:<thread>`
  // for forum threads) — same shape normalizeTelegram() emits. Earlier we
  // were writing just String(chat.id), which then failed downstream at
  // sendOutbound → telegramSessionToChatAndThread with
  // "Invalid telegram sessionId: <number>".
  const threadId: number | undefined =
    typeof message?.message_thread_id === "number"
      ? message.message_thread_id
      : undefined;
  const sessionId = threadId
    ? `telegram:${chat.id}:${threadId}`
    : `telegram:${chat.id}`;

  // Mirror normalize.ts: capture message_id + reply binding so the chat
  // agent can ground voice-note replies via the tgmsg side table too.
  const tgMessageId =
    typeof message?.message_id === "number" ? message.message_id : undefined;
  const replyToTgMessageId =
    typeof message?.reply_to_message?.message_id === "number"
      ? message.reply_to_message.message_id
      : undefined;

  return {
    channel: "telegram",
    sessionId,
    senderId: String(from.id ?? chat.id),
    senderUsername: from.username ? String(from.username) : undefined,
    text,
    ts: typeof message.date === "number" ? message.date * 1000 : Date.now(),
    raw: {
      ...update,
      transcribedMedia: {
        provider: "telegram",
        kind: media.kind,
        mimeType: media.mimeType,
        duration: media.duration,
        fileSize: media.fileSize,
      },
    },
    tgMessageId,
    replyToTgMessageId,
  };
}

async function normalizeTelegramWithTranscription(update: any): Promise<InboundMessage | null> {
  const media = extractTelegramAudioMedia(update);

  // Preserve existing behavior for normal text/photo/etc.
  if (!media) {
    return normalizeTelegram(update);
  }

  const base = await normalizeTelegram(update).catch(() => null);
  const transcript = await transcribeTelegramAudio(media);

  if (!transcript) {
    const fallbackText =
      "I received your voice message, but I could not transcribe any speech from it.";

    return base
      ? {
          ...base,
          text: fallbackText,
          raw: {
            ...(base.raw as any),
            transcribedMedia: {
              provider: "telegram",
              kind: media.kind,
              mimeType: media.mimeType,
              duration: media.duration,
              fileSize: media.fileSize,
              emptyTranscript: true,
            },
          },
        }
      : buildTelegramInboundFromUpdate(update, fallbackText, media);
  }

  const existingText = base?.text?.trim();

  const finalText = existingText
    ? `${existingText}\n\n[Voice transcript]\n${transcript}`
    : transcript;

  return base
    ? {
        ...base,
        text: finalText,
        raw: {
          ...(base.raw as any),
          transcribedMedia: {
            provider: "telegram",
            kind: media.kind,
            mimeType: media.mimeType,
            duration: media.duration,
            fileSize: media.fileSize,
            transcript,
          },
        },
      }
    : buildTelegramInboundFromUpdate(update, finalText, media);
}

async function handleTelegramWebhook(req: Request) {
  if (!(await telegramValidateWebhook(req))) {
    return new Response("Unauthorized", { status: 401 });
  }

  const update = await req.json().catch(() => null);
  if (!update) return new Response("Bad JSON", { status: 400 });

  const updateId = (update as any)?.update_id;

  if (typeof updateId === "number") {
    const store = getStore();
    const key = `dedupe:telegram:update:${updateId}`;
    const inserted = await store.set(key, "1", {
      exSeconds: 600,
      nx: true,
    });

    if (!inserted) return jsonOk({ deduped: true });
  }

  // Voice / audio path: transcription via Whisper takes 5-15s. Synchronously
  // awaiting blew past Telegram's webhook read-timeout, which we observed
  // as `last_error_message: "Read timeout expired"`. Defer all that work
  // via Vercel's waitUntil so we return 200 to Telegram immediately and the
  // function keeps running until transcription + handleInbound finish.
  const media = extractTelegramAudioMedia(update);
  if (media) {
    console.log(
      `[telegram] voice update ${updateId}: ${media.kind} (${media.mimeType}, ${media.fileSize ?? "?"}B) — handling in background`
    );
    waitUntil(handleVoiceUpdateBackground(update, media));
    return jsonOk({ queued: "voice" });
  }

  // Text / photo path is fast — keep synchronous so any thrown error bubbles
  // back to Telegram (it'll retry the update).
  try {
    const msg = await normalizeTelegram(update);
    if (msg) await handleInbound(msg);
    return jsonOk();
  } catch (err: any) {
    console.error("[telegram] inbound handling failed", err);
    return jsonOk({ error: err?.message ?? "Unknown error" });
  }
}

// Background voice handler — invoked via waitUntil so the Telegram webhook
// can return 200 immediately. Errors here can't reach Telegram, so we
// surface them via a user-facing fallback message and console.error logs.
async function handleVoiceUpdateBackground(
  update: any,
  media: TelegramAudioMedia
): Promise<void> {
  try {
    const msg = await normalizeTelegramWithTranscription(update);
    if (msg) {
      console.log(
        `[telegram] voice transcribed (${msg.text.length} chars), starting workflow`
      );
      await handleInbound(msg);
    } else {
      console.warn(
        `[telegram] voice transcription returned null msg — update ${update?.update_id}`
      );
    }
  } catch (err: any) {
    console.error("[telegram] background voice handling failed", err);
    const fallback = buildTelegramInboundFromUpdate(
      update,
      "I received your voice message, but transcription failed. Please resend or type the message.",
      media
    );
    if (fallback) {
      try {
        await handleInbound(fallback);
      } catch (sendErr) {
        console.error(
          "[telegram] voice failure fallback also failed",
          sendErr
        );
      }
    }
  }
}

// ============================================================
// Pairing
// ============================================================
async function maybeHandleChatPairingCommand(msg: InboundMessage): Promise<boolean> {
  const envAllowConfigured =
    (msg.channel === "telegram" && process.env.TELEGRAM_ALLOWED_USERS != null) ||
    (msg.channel === "whatsapp" && process.env.WHATSAPP_ALLOWED_NUMBERS != null) ||
    (msg.channel === "sms" && process.env.SMS_ALLOWED_NUMBERS != null);

  if (envAllowConfigured) return false;

  const cmd = parsePairCommand(msg.text);
  if (!cmd) return false;

  const identity = makeIdentity(msg.channel, msg.senderId);

  if (!cmd.code) {
    const pending = await getPendingCode(identity);

    if (pending) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `Pending pairing code: ${pending}\nReply with /pair ${pending}`,
      });
    } else {
      const code = await createPairing(identity);

      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `Pairing code: ${code}\nReply with /pair ${code}`,
      });
    }

    return true;
  }

  const ok = await approvePairing(identity, cmd.code);

  await sendOutboundRuntime({
    channel: msg.channel,
    sessionId: msg.sessionId,
    text: ok ? "✅ Paired. You can now use the bot." : "❌ Invalid or expired pairing code.",
  });

  return true;
}

// ============================================================
// Workflow routing
// ============================================================
async function routeToSession(msg: InboundMessage): Promise<void> {
  await start(sessionWorkflow, [msg.sessionId, msg]);
}

// The effective tenant namespace a message operates on. Set at ingress
// (handleInbound) to the shared team namespace when the message arrives in a
// bound team group; otherwise the per-user identity. All command handlers scope
// to this so /job, /code, /automate, memory, VFS, etc. share within a team.
function tenantOf(msg: InboundMessage): string {
  return msg.tenantId ?? makeIdentity(msg.channel, msg.senderId);
}

// The Telegram group chat id embedded in a sessionId, or null for a DM/other.
// sessionId is `telegram:<chatId>[:<threadId>]`; group/supergroup chat ids are
// negative, DMs are the (positive) user id — so a bound team only ever matches
// a real group.
function telegramGroupChatId(msg: InboundMessage): string | null {
  if (msg.channel !== "telegram") return null;
  const chatId = msg.sessionId.split(":")[1];
  return chatId && chatId.startsWith("-") ? chatId : null;
}

// If this message is in a Telegram group bound to a team, return that team's
// tenant namespace and lazily record the sender in the roster; else null.
async function resolveTeamTenant(msg: InboundMessage): Promise<string | null> {
  const chatId = telegramGroupChatId(msg);
  if (!chatId) return null;
  // Hot path: cached binding lookup, then atomic roster ops — no full-record
  // read/write (which would race across concurrent group messages).
  const teamId = await teamIdForGroupChat(chatId);
  if (!teamId) return null;
  const userTenant = makeIdentity(msg.channel, msg.senderId);
  // Someone who explicitly /workspace-left stays in their personal namespace
  // even while still in the Telegram group — and must not be auto re-enrolled.
  if (await hasUserLeft(teamId, userTenant)) return null;
  // Being in the bound Telegram group is the access grant; record membership so
  // the roster (/workspace members) reflects who actually participates.
  try {
    await recordMemberAuto(teamId, {
      tenantId: userTenant,
      senderId: msg.senderId,
      username: msg.senderUsername,
    });
    // Opportunistic @username → DM-session index so invites can DM by handle.
    // Always the user's PRIVATE chat (telegram:<userId>) — never this group's
    // session, or invites would post into the group instead of a DM.
    if (msg.senderUsername) {
      await getStore().set(
        `tg:uname:${msg.senderUsername.toLowerCase()}`,
        `telegram:${msg.senderId}`
      );
    }
  } catch (err) {
    // Roster bookkeeping is best-effort; never block the message — but make the
    // drift visible (user has team access without appearing in /workspace members).
    console.warn(`[teams] roster update failed for ${teamId}: ${String(err)}`);
  }
  return teamTenantId(teamId);
}

// Build a shareable link that lets someone join a team. Prefers a real Telegram
// group invite link (needs the bot to be a group admin with invite rights);
// falls back to a bot deep-link that opens a DM carrying the join token.
async function buildTeamInviteLink(
  team: Team
): Promise<{ link: string; kind: "group" | "deeplink" }> {
  if (team.tgGroupChatId) {
    try {
      const link = await telegramCreateInviteLink(team.tgGroupChatId, { name: team.name });
      return { link, kind: "group" };
    } catch {
      // bot isn't an admin / can't create — fall through to the deep link
    }
  }
  const me = await telegramGetMe().catch(() => null);
  const link = me?.username
    ? `https://t.me/${me.username}?start=team_${team.inviteToken}`
    : `/join ${team.inviteToken}`;
  return { link, kind: "deeplink" };
}

// Resolve an invite target ("@handle" or numeric telegram user id) to a DM
// sessionId we can message, or null when we have no prior contact with them.
async function resolveInviteTargetSession(who: string): Promise<string | null> {
  const w = who.trim();
  if (/^\d+$/.test(w)) return `telegram:${w}`; // numeric user id → DM session
  const uname = w.replace(/^@/, "").toLowerCase();
  if (uname) {
    const sid = await getStore().get<string>(`tg:uname:${uname}`);
    if (sid) return sid;
  }
  return null;
}

// Handle a team join deep-link (/start team_<token> or /join <token>). Adds the
// tapping user to the roster and replies (in their DM) with the group chat link.
async function handleWorkspaceJoin(msg: InboundMessage, token: string): Promise<void> {
  const reply = (text: string) =>
    sendOutboundRuntime({ channel: msg.channel, sessionId: msg.sessionId, text });
  const team = await getTeamByInviteToken(token);
  if (!team) {
    await reply("❌ That team invite link is invalid or has expired.");
    return;
  }
  await addTeamMember(team.id, {
    tenantId: makeIdentity(msg.channel, msg.senderId),
    senderId: msg.senderId,
    username: msg.senderUsername,
  });
  const { link, kind } = await buildTeamInviteLink(team);
  await reply(
    kind === "group"
      ? `🤝 You're on team "${team.name}". Join the shared group chat here — that's where the shared bot, accounts and automations live:\n${link}`
      : `🤝 You've joined team "${team.name}". Ask the owner to add you to the group chat (they can /workspace link) so you can use the shared workspace.`
  );
}

// Test the tenant's enabled chat-pattern automations against an inbound
// message and fire each match. Non-blocking by design (called fire-and-forget)
// so the normal conversational reply is never delayed.
async function fireMatchingChatAutomations(msg: InboundMessage): Promise<void> {
  const text = (msg.text ?? "").trim();
  if (!text || text.startsWith("/")) return; // never match slash commands
  const tenantId = tenantOf(msg);
  const rules = await listAutomationsByTenant(tenantId);
  for (const rule of rules) {
    if (!rule.enabled || rule.trigger.kind !== "chat") continue;
    // Bounded: invalid OR ReDoS-shaped patterns are skipped, and the tested
    // input is length-capped, so one tenant's rule can't hang the shared loop.
    const re = safeCompileRegex(rule.trigger.pattern, rule.trigger.flags ?? "i");
    if (!re) continue;
    if (safeRegexTest(re, text)) {
      await fireAutomation(rule.id, "chat", { text, channel: msg.channel });
    }
  }
}

// Dispatch a long-running job. Returns the jobId immediately; the workflow
// runs asynchronously and writes thoughts/result to Redis as it goes.
//
// If `forceDeep` is true (set by /deep or /extended command), the classifier
// is bypassed. Otherwise: heuristics first, LLM tiebreaker only if uncertain.
async function dispatchJob(args: {
  channel: Channel;
  sessionId: string;
  senderId: string;
  prompt: string;
  forceDeep?: boolean;
  // Explicit tenant namespace override — set to the team namespace for jobs
  // launched from a team group so the run + its artifacts are shared.
  tenantId?: string;
}): Promise<{ jobId: string; deep: boolean; reason: string }> {
  const tenantId = args.tenantId ?? makeIdentity(args.channel, args.senderId);

  const decision = await classifyDepth(args.prompt, {
    explicit: args.forceDeep,
  });

  const meta = await createJob({
    tenantId,
    channel: args.channel,
    sessionId: args.sessionId,
    prompt: args.prompt,
    kind: decision.deep ? "research" : "auto",
  });

  // Stash the depth decision so the workflow can read it without re-running
  // the classifier and the user can see WHY their job is deep in the log.
  const { appendThought, updateJobMeta } = await import("@/app/lib/jobStore");
  await updateJobMeta(meta.jobId, {
    kind: decision.deep ? "research" : "auto",
  });
  await appendThought(meta.jobId, {
    kind: "info",
    text: `depth: ${decision.deep ? "DEEP" : "normal"} — ${decision.reason}`,
    data: { deep: decision.deep, source: decision.source },
  });

  await start(jobWorkflow, [meta.jobId]);
  await recordActivity(tenantId, {
    kind: "job",
    summary: `dispatched ${decision.deep ? "deep" : "normal"} job: ${args.prompt.slice(0, 100)}`,
    meta: { jobId: meta.jobId, deep: decision.deep, reason: decision.reason },
  });
  // Mirror to the audit log so /ui's Activity panel (which filters to
  // tool.* kinds) surfaces job dispatches the same way it surfaces
  // /code dispatches.
  await recordAudit(tenantId, {
    kind: "tool.job_dispatch",
    summary: `/job ${decision.deep ? "(deep) " : ""}${meta.jobId}: ${args.prompt.slice(0, 120)}`,
    meta: {
      jobId: meta.jobId,
      deep: decision.deep,
      reason: decision.reason,
    },
  });
  return { jobId: meta.jobId, deep: decision.deep, reason: decision.reason };
}

// Materialize a compiled automation spec into a stored Automation and register
// its trigger side (schedule ZSET entry / Composio subscription / minted
// webhook secret). Returns the saved rule plus a human-readable registration
// note (e.g. the webhook URL or composio subscription id).
// registerAutomation moved to app/lib/registerAutomation.ts — shared with the
// chat agent's create_workforce tool.

// Parses `/job <prompt>` (with optional leading whitespace). Returns the
// prompt body if matched, else null.
function parseJobCommand(text: string): string | null {
  const trimmed = (text ?? "").trim();
  if (!/^\/job(\s+|$)/i.test(trimmed)) return null;
  const body = trimmed.replace(/^\/job\s*/i, "").trim();
  return body.length > 0 ? body : null;
}

// Parses `/deep <prompt>` or `/extended <prompt>` — same as /job but forces
// deep / pro-extended mode regardless of classifier output.
function parseDeepCommand(text: string): string | null {
  const trimmed = (text ?? "").trim();
  if (!/^\/(deep|extended)(\s+|$)/i.test(trimmed)) return null;
  const body = trimmed.replace(/^\/(deep|extended)\s*/i, "").trim();
  return body.length > 0 ? body : null;
}

// Parses `/status <jobId>` for inline job inspection from Telegram.
function parseStatusCommand(text: string): string | null {
  const trimmed = (text ?? "").trim();
  if (!/^\/status(\s+|$)/i.test(trimmed)) return null;
  const body = trimmed.replace(/^\/status\s*/i, "").trim();
  return body.length > 0 ? body : null;
}

// Parses `/ask <jobId> <question...>` — read-only side-channel inspection.
function parseAskCommand(
  text: string
): { jobId: string; question: string } | null {
  const trimmed = (text ?? "").trim();
  if (!/^\/ask(\s+|$)/i.test(trimmed)) return null;
  const body = trimmed.replace(/^\/ask\s*/i, "").trim();
  const m = body.match(/^(\S+)\s+(.+)$/s);
  if (!m) return null;
  return { jobId: m[1], question: m[2].trim() };
}

// Parses `/logins` — list saved browser sessions for the user.
function isLoginsCommand(text: string): boolean {
  return /^\/logins\s*$/i.test((text ?? "").trim());
}

// Parses `/forget <hostname>` or `/forget all` — purge saved sessions.
function parseForgetCommand(text: string): { target: string } | null {
  const trimmed = (text ?? "").trim();
  if (!/^\/forget(\s+|$)/i.test(trimmed)) return null;
  const body = trimmed.replace(/^\/forget\s*/i, "").trim();
  if (!body) return null;
  return { target: body };
}

// Memory + trigger parsers --------------------------------------------------

function parseRememberCommand(text: string): {
  kind: "fact" | "summarize_chat";
  body: string;
  topic?: string;
} | null {
  const m = (text ?? "").trim().match(/^\/remember(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body) return null;

  // Subcommand triggers for chat summarization:
  //   /remember chat
  //   /remember today
  //   /remember session
  //   /remember our chat
  //   /remember everything
  //   /remember all of our conversation
  const lowered = body.toLowerCase();
  const SUMMARY_INTENTS = [
    /^chat\b/,
    /^today('?s)?\s*chat\b/,
    /^today\b\s*$/,
    /^session\b/,
    /^our\s+(chat|conversation)\b/,
    /^the\s+(chat|conversation)\b/,
    /^this\s+(chat|conversation|session)\b/,
    /^everything\s+(we|i)\s+(said|discussed|talked)\b/,
    /^all\s+of\s+(our|the)\s+(chat|conversation)\b/,
  ];
  for (const re of SUMMARY_INTENTS) {
    if (re.test(lowered)) {
      // Anything trailing after "about X" becomes a topic hint.
      const topicMatch = lowered.match(/\babout\s+(.+)$/);
      return {
        kind: "summarize_chat",
        body,
        topic: topicMatch ? topicMatch[1].trim() : undefined,
      };
    }
  }
  return { kind: "fact", body };
}
function isLogsCommand(text: string): boolean {
  const t = (text ?? "").trim();
  return /^\/(logs|activity)\s*$/i.test(t);
}
function isFavoritesCommand(text: string): boolean {
  return /^\/favorites\s*$/i.test((text ?? "").trim());
}

// /autopilot on | off | status
function parseAutopilotCommand(text: string): "on" | "off" | "status" | null {
  const m = (text ?? "").trim().match(/^\/autopilot(?:\s+|$)(\S*)/i);
  if (!m) return null;
  const v = (m[1] ?? "").toLowerCase();
  if (v === "on" || v === "enable") return "on";
  if (v === "off" || v === "disable") return "off";
  if (v === "" || v === "status") return "status";
  return null;
}

// /debug on | off | status
function parseDebugCommand(text: string): "on" | "off" | "status" | null {
  const m = (text ?? "").trim().match(/^\/debug(?:\s+|$)(\S*)/i);
  if (!m) return null;
  const v = (m[1] ?? "").toLowerCase();
  if (v === "on" || v === "enable") return "on";
  if (v === "off" || v === "disable") return "off";
  if (v === "" || v === "status") return "status";
  return null;
}

function isMemoriesCommand(text: string): boolean {
  return /^\/memories\s*$/i.test((text ?? "").trim());
}
function parseMemForgetCommand(
  text: string
): { id?: string; tag?: string } | null {
  const m = (text ?? "").trim().match(/^\/memforget(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body) return null;
  if (body.startsWith("tag:")) return { tag: body.slice(4).trim() };
  return { id: body };
}

function parseSubscribeCommand(text: string): {
  slug: string;
  configJson?: string;
} | null {
  const m = (text ?? "").trim().match(/^\/subscribe(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body) return null;
  // First whitespace-delimited token is the slug; rest (if it begins with
  // `{`) is parsed as JSON trigger config.
  const parts = body.split(/\s+/);
  const slug = parts[0];
  const rest = body.slice(slug.length).trim();
  if (rest.startsWith("{")) return { slug, configJson: rest };
  return { slug };
}
function isTriggersCommand(text: string): boolean {
  return /^\/triggers\s*$/i.test((text ?? "").trim());
}
function parseUnsubscribeCommand(text: string): string | null {
  const m = (text ?? "").trim().match(/^\/unsubscribe(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  return body.length > 0 ? body : null;
}

// Automations ("flows") -----------------------------------------------------
//
//   /automate <free-form description>   compile + register a new automation
//   /automate list                      list this tenant's automations
//   /automate pause|resume|delete|run <id>
//   /automations                        alias for `/automate list`
type AutomateCommand =
  | { sub: "create"; spec: string }
  | { sub: "list" }
  | { sub: "pause" | "resume" | "delete" | "run"; id: string };

function parseAutomateCommand(text: string): AutomateCommand | null {
  const trimmed = (text ?? "").trim();
  if (/^\/automations\s*$/i.test(trimmed)) return { sub: "list" };
  const m = trimmed.match(/^\/automate(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body || /^list$/i.test(body)) return { sub: "list" };
  const sub = body.match(/^(pause|resume|delete|run)\s+(\S+)\s*$/i);
  if (sub) {
    return {
      sub: sub[1].toLowerCase() as "pause" | "resume" | "delete" | "run",
      id: sub[2],
    };
  }
  return { sub: "create", spec: body };
}

function automationTriggerLabel(t: AutomationTrigger): string {
  switch (t.kind) {
    case "schedule":
      return t.cron ? `schedule (cron ${t.cron})` : `schedule (every ${Math.round((t.everyMs ?? 0) / 1000)}s)`;
    case "composio":
      return `event ${t.triggerType}`;
    case "webhook":
      return "webhook";
    case "chat":
      return `chat /${t.pattern}/${t.flags ?? "i"}`;
  }
}

// Sub-agents + workforces -----------------------------------------------------
//
//   /agent create <name> | toolkits: a,b | persona: ...   (or pure NL)
//   /agents                                  list this tenant's sub-agents
//   /agent delete <id>
//   /agent bind <id> <botToken>              attach a dedicated Telegram bot
//   /team create <free-form description>     compile + register a workforce
//   /teams                                   list workforces
//   /team run|pause|resume|delete <id>
type AgentCommand =
  | { sub: "create"; spec: string }
  | { sub: "list" }
  | { sub: "delete"; id: string }
  | { sub: "bind"; id: string; token: string }
  | { sub: "optimize"; id: string };

function parseAgentCommand(text: string): AgentCommand | null {
  const trimmed = (text ?? "").trim();
  if (/^\/agents\s*$/i.test(trimmed)) return { sub: "list" };
  const m = trimmed.match(/^\/agent(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body || /^list$/i.test(body)) return { sub: "list" };
  const del = body.match(/^delete\s+(\S+)\s*$/i);
  if (del) return { sub: "delete", id: del[1] };
  const bind = body.match(/^bind\s+(\S+)\s+(\S+)\s*$/i);
  if (bind) return { sub: "bind", id: bind[1], token: bind[2] };
  const opt = body.match(/^optimize\s+(\S+)\s*$/i);
  if (opt) return { sub: "optimize", id: opt[1] };
  const create = body.match(/^create\s+([\s\S]+)$/i);
  if (create) return { sub: "create", spec: create[1].trim() };
  return null;
}

// Structured form: `Name | toolkits: gmail, slack | persona: ...`. Returns
// null when the spec doesn't follow the pipe syntax (→ NL compile fallback).
function parseStructuredAgentSpec(
  spec: string
): { name: string; emoji: string; persona: string; toolkits: string[] } | null {
  if (!spec.includes("|")) return null;
  const parts = spec.split("|").map((p) => p.trim());
  const name = parts[0];
  if (!name) return null;
  let toolkits: string[] = [];
  let persona = "";
  let emoji = "🤖";
  for (const p of parts.slice(1)) {
    const kv = p.match(/^(toolkits?|persona|emoji)\s*:\s*([\s\S]*)$/i);
    if (!kv) continue;
    const key = kv[1].toLowerCase();
    const val = kv[2].trim();
    if (key.startsWith("toolkit")) {
      toolkits = val.split(/[,\s]+/).map((t) => t.trim().toLowerCase()).filter(Boolean);
    } else if (key === "persona") {
      persona = val;
    } else if (key === "emoji") {
      emoji = val || emoji;
    }
  }
  return { name, emoji, persona: persona || `Specialist named ${name}.`, toolkits };
}

type TeamCommand =
  | { sub: "create"; spec: string }
  | { sub: "list" }
  | { sub: "run" | "pause" | "resume" | "delete"; id: string };

function parseTeamCommand(text: string): TeamCommand | null {
  const trimmed = (text ?? "").trim();
  if (/^\/teams\s*$/i.test(trimmed)) return { sub: "list" };
  const m = trimmed.match(/^\/team(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body || /^list$/i.test(body)) return { sub: "list" };
  const sub = body.match(/^(run|pause|resume|delete)\s+(\S+)\s*$/i);
  if (sub) {
    return { sub: sub[1].toLowerCase() as "run" | "pause" | "resume" | "delete", id: sub[2] };
  }
  const create = body.match(/^create\s+([\s\S]+)$/i);
  if (create) return { sub: "create", spec: create[1].trim() };
  // Bare `/team <description>` reads as create, matching /automate ergonomics.
  return { sub: "create", spec: body };
}

// --- /workspace (shared multi-user Team namespace) --------------------------
//
// Distinct from /team, which builds an AI-agent workforce. A "workspace" is a
// shared tenant namespace: multiple humans in a bound Telegram group share one
// set of Composio connections, automations, jobs, /code projects, VFS + memory.
type WorkspaceCommand =
  | { sub: "create"; name: string }
  | { sub: "invite"; who: string }
  | { sub: "link" }
  | { sub: "list" }
  | { sub: "members" }
  | { sub: "rename"; name: string }
  | { sub: "leave" }
  | { sub: "delete" }
  | { sub: "help" }
  | { sub: "join"; token: string }
  // Re-bind an existing team to THIS group — recovery for Telegram's silent
  // group→supergroup migration, which changes the chat id and orphans the team.
  | { sub: "rebind"; teamId: string };

function parseWorkspaceCommand(text: string): WorkspaceCommand | null {
  const trimmed = (text ?? "").trim();
  const m = trimmed.match(/^\/(?:workspace|ws)(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;
  const body = (m[1] ?? "").trim();
  if (!body || /^help$/i.test(body)) return { sub: "help" };
  if (/^list$/i.test(body)) return { sub: "list" };
  if (/^members$/i.test(body)) return { sub: "members" };
  if (/^link$/i.test(body)) return { sub: "link" };
  if (/^leave$/i.test(body)) return { sub: "leave" };
  if (/^delete$/i.test(body)) return { sub: "delete" };
  const create = body.match(/^create\s+([\s\S]+)$/i);
  if (create) return { sub: "create", name: create[1].trim() };
  const invite = body.match(/^invite\s+([\s\S]+)$/i);
  if (invite) return { sub: "invite", who: invite[1].trim() };
  const rename = body.match(/^rename\s+([\s\S]+)$/i);
  if (rename) return { sub: "rename", name: rename[1].trim() };
  const rebind = body.match(/^rebind\s+(\S+)\s*$/i);
  if (rebind) return { sub: "rebind", teamId: rebind[1] };
  return { sub: "help" };
}

// A team join deep-link: `/start team_<token>` (tapped from an invite) or
// `/join <token>`. Parsed separately because it must run before the /start
// stop-toggle at ingress. Returns the token or null.
function parseWorkspaceJoinToken(text: string): string | null {
  const m = (text ?? "")
    .trim()
    .match(/^(?:\/start\s+team_|\/join\s+)([A-Za-z0-9_-]+)\s*$/i);
  return m ? m[1] : null;
}

function describeStages(stages: WorkforceStage[], agentName: (id: string) => string): string {
  return stages
    .map((s, i) => {
      if (s.kind === "route") {
        return `${i + 1}. 🧭 route (pick ${s.maxPick ?? 1} of: ${s.candidateAgentIds.map(agentName).join(", ")})`;
      }
      return `${i + 1}. ${s.agentIds.map(agentName).join(" + ")}`;
    })
    .join("\n");
}

// Parses `/code …` slash-command surface for long-running coding projects.
//
// Subcommands:
//   /code <task>                              start a new project, returns PROJECT_ID
//   /code attach <projectId> <task>           continue an existing project with a follow-up
//   /code status <projectId>                  show recent log + last output
//   /code list                                list active + recent projects for this tenant
//   /code push <projectId> [repoUrl] [branch] materialize project to GitHub
//
// New-project inline flags (any order, before the body):
//   repo:<url>     clone this GitHub repo and operate inside it
//   branch:<name>  start from this branch
//   engine:<name>  force engine: claude | opencode (default: auto)
//
// Aliases: `/cc` and `/claude` work the same as `/code`.
type ClaudeCodeCommand =
  | {
      sub: "new";
      task: string;
      repoUrl?: string;
      baseBranch?: string;
      engineOverride?: CodeEngine;
    }
  | { sub: "attach"; projectId: string; task: string }
  | { sub: "status"; projectId: string }
  | { sub: "list" }
  | { sub: "push"; projectId: string; repoUrl?: string; branch?: string };

function parseClaudeCodeCommand(text: string): ClaudeCodeCommand | null {
  const trimmed = (text ?? "").trim();
  const m = trimmed.match(/^\/(code|cc|claude)(?:\s+|$)([\s\S]*)$/i);
  if (!m) return null;

  let body = (m[2] ?? "").trim();
  if (!body) return null;

  // Subcommand dispatch — keyword has to be the first whitespace-delimited
  // token so a "/code attach the dark mode css to..." style task isn't
  // accidentally hijacked.
  const subMatch = body.match(/^(attach|status|list|push)(\s+|$)([\s\S]*)$/i);
  if (subMatch) {
    const sub = subMatch[1].toLowerCase();
    const rest = (subMatch[3] ?? "").trim();
    if (sub === "list") return { sub: "list" };
    if (sub === "status") {
      const pid = rest.split(/\s+/)[0];
      return pid ? { sub: "status", projectId: pid } : null;
    }
    if (sub === "attach") {
      const pid = rest.split(/\s+/)[0];
      const followup = rest.slice(pid.length).trim();
      if (!pid || !followup) return null;
      return { sub: "attach", projectId: pid, task: followup };
    }
    if (sub === "push") {
      const parts = rest.split(/\s+/);
      const pid = parts[0];
      if (!pid) return null;
      let repoUrl: string | undefined;
      let branch: string | undefined;
      for (const tok of parts.slice(1)) {
        if (/^https?:\/\//i.test(tok)) repoUrl = tok;
        else if (tok) branch = tok;
      }
      return { sub: "push", projectId: pid, repoUrl, branch };
    }
  }

  // New-project flow. Pull inline flags off the head until none match.
  let repoUrl: string | undefined;
  let baseBranch: string | undefined;
  let engineOverride: CodeEngine | undefined;
  const flagRe = /^(repo:(\S+)|branch:(\S+)|engine:(\S+))(?:\s+|$)/i;
  while (true) {
    const flag = body.match(flagRe);
    if (!flag) break;
    if (flag[2]) repoUrl = flag[2];
    else if (flag[3]) baseBranch = flag[3];
    else if (flag[4]) {
      const e = flag[4].toLowerCase();
      if (e === "claude" || e === "opencode") engineOverride = e as CodeEngine;
    }
    body = body.slice(flag[0].length).trim();
  }

  // If no explicit repo: flag, fall back to scanning the body for a bare
  // GitHub URL. Users frequently paste `/code implement X
  // https://github.com/owner/repo` and reasonably expect us to clone the
  // repo before invoking the engine. Without this auto-detect, the URL
  // stays in the task text and the engine runs against an empty workdir
  // (so it hallucinates an "implementation" instead of editing real code).
  if (!repoUrl) {
    const m = body.match(
      /https?:\/\/github\.com\/[A-Za-z0-9._-]+\/[A-Za-z0-9._-]+(?:\.git)?(?=$|\s|[?#])/i
    );
    if (m) {
      repoUrl = m[0];
      // Strip the URL from the task body so the engine prompt isn't
      // littered with it (and we don't double-prompt the engine to "go to"
      // the URL when we've already cloned it locally).
      body = body.replace(m[0], "").replace(/\s{2,}/g, " ").trim();
    }
  }

  if (!body) return null;
  return {
    sub: "new",
    task: body,
    repoUrl,
    baseBranch,
    engineOverride,
  };
}

// ============================================================
// Inbound handling
// ============================================================
async function handleInbound(msg: InboundMessage): Promise<void> {
  if (await maybeHandleChatPairingCommand(msg)) return;

  // Team join deep-link (/start team_<token> or /join <token>) — must run before
  // the /start stop-toggle below, which would otherwise swallow it.
  {
    const joinToken = parseWorkspaceJoinToken(msg.text);
    if (joinToken) {
      await handleWorkspaceJoin(msg, joinToken);
      return;
    }
  }

  // Team namespace remap: a message in a bound team GROUP chat operates on the
  // shared team tenant (team:<id>) instead of the per-user identity, which is
  // what makes Composio connections, automations, jobs, /code, VFS and memory
  // shared across the team. Group membership is itself the access grant, so
  // members don't each need to be on the operator allowlist. Computed BEFORE
  // /stop so a halt cancels the right tenant's in-flight work.
  const teamTenant = await resolveTeamTenant(msg);
  if (teamTenant) msg.tenantId = teamTenant;

  // HARD /stop + /start at ingress (no LLM; no workflow).
  //   /stop  — halt everything: stop typing indicators and cancel any running
  //            jobs/deep-jobs, then stay quiet until /start.
  //   /start — a fresh reboot: clear the halt, cancel leftover work, and wipe
  //            this chat's conversation context so the next turn starts clean.
  {
    if (isStopCmd(msg.text)) {
      await setChatStopped(msg.channel, msg.sessionId);
      const cancelled = await cancelActiveJobs(tenantOf(msg), "halted by /stop");
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          `🛑 Stopped — halted typing${
            cancelled.length ? ` and cancelled ${cancelled.length} running task(s)` : ""
          }. Send /start to reboot.`,
      });
      return;
    }

    if (isStartCmd(msg.text)) {
      await clearChatStopped(msg.channel, msg.sessionId);
      // Reboot: cancel anything still in flight and wipe the conversation
      // history for this chat so context starts fresh.
      const cancelled = await cancelActiveJobs(tenantOf(msg), "halted by /start (reboot)");
      await getStore().del(`sess:${msg.sessionId}:history`);
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          `🔄 Fresh start — cleared this chat's context${
            cancelled.length ? ` and stopped ${cancelled.length} running task(s)` : ""
          }. What do you want to do?`,
      });
      return;
    }

    if (await isChatStopped(msg.channel, msg.sessionId)) return;
  }

  const allowed = teamTenant
    ? ({ allowed: true } as Awaited<ReturnType<typeof isInboundAllowed>>)
    : await isInboundAllowed(msg);

  await saveSessionMeta(
    {
      channel: msg.channel,
      sessionId: msg.sessionId,
      senderId: msg.senderId,
      senderUsername: msg.senderUsername,
      updatedAt: Date.now(),
    },
    {
      updateLast: allowed.allowed,
    }
  );

  // @username → DM-session index (best-effort) so /workspace invite can DM by
  // handle anyone who has ever messaged the bot, not just team-group members.
  if (msg.channel === "telegram" && msg.senderUsername) {
    try {
      await getStore().set(
        `tg:uname:${msg.senderUsername.toLowerCase()}`,
        `telegram:${msg.senderId}`
      );
    } catch {
      // index write must never block the message
    }
  }

  if (!allowed.allowed) {
    const hasTelegramAllow = process.env.TELEGRAM_ALLOWED_USERS != null;
    const hasWhatsAllow = process.env.WHATSAPP_ALLOWED_NUMBERS != null;
    const hasSmsAllow = process.env.SMS_ALLOWED_NUMBERS != null;

    const identity = makeIdentity(msg.channel, msg.senderId);

    if (
      (msg.channel === "telegram" && hasTelegramAllow) ||
      (msg.channel === "whatsapp" && hasWhatsAllow) ||
      (msg.channel === "sms" && hasSmsAllow)
    ) {
      const hint =
        msg.channel === "telegram"
          ? `Set TELEGRAM_ALLOWED_USERS to include: ${msg.senderId}${
              msg.senderUsername ? ` or @${msg.senderUsername}` : ""
            }`
          : msg.channel === "whatsapp"
            ? `Set WHATSAPP_ALLOWED_NUMBERS to include: ${msg.senderId} (E.164)`
            : `Set SMS_ALLOWED_NUMBERS to include: ${msg.senderId} (E.164)`;

      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `🔒 Unauthorized (${allowed.reason ?? "not allowed"}).\nIdentity: ${identity}\n\nOperator hint: ${hint}`,
      });

      return;
    }

    const pending = await getPendingCode(identity);
    const code = pending ?? (await createPairing(identity));

    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text:
        `🔒 This bot is locked.\n` +
        `Reply with: /pair ${code}\n` +
        `This code expires in 15 minutes.`,
    });

    return;
  }

  // /deep <prompt> or /extended <prompt> — force pro-extended deep mode.
  const deepPrompt = parseDeepCommand(msg.text);
  if (deepPrompt) {
    const out = await dispatchJob({
      channel: msg.channel,
      sessionId: msg.sessionId,
      senderId: msg.senderId,
      prompt: deepPrompt,
      forceDeep: true,
      tenantId: msg.tenantId,
    });
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: `on it, deep mode. ${out.jobId} — I'll ping you when it lands. /status ${out.jobId} to peek, /ask ${out.jobId} <q> to interrupt with a question.`,
    });
    return;
  }

  // /job <prompt> — auto-detect depth; deep if heuristics+classifier say so.
  const jobPrompt = parseJobCommand(msg.text);
  if (jobPrompt) {
    const out = await dispatchJob({
      channel: msg.channel,
      sessionId: msg.sessionId,
      senderId: msg.senderId,
      prompt: jobPrompt,
      tenantId: msg.tenantId,
    });
    const tail = out.deep
      ? `running ${out.jobId} in deep mode. I'll come back with the answer.`
      : `got it. ${out.jobId} kicked off. /status ${out.jobId} for progress.`;
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: tail,
    });
    return;
  }

  // /remember <text>  OR  /remember (chat|today|our conversation|…) [about <topic>]
  const rememberCmd = parseRememberCommand(msg.text);
  if (rememberCmd) {
    const tenantId = tenantOf(msg);

    if (rememberCmd.kind === "summarize_chat") {
      // Chat-summary path: pull session history, ask gpt-4o for a structured
      // summary + atomic items, persist them. Mirrors the in-agent tool but
      // routed directly from the slash command so the user doesn't have to
      // wait for the agent to choose the tool.
      try {
        const history = (await loadHistoryStep(msg.sessionId)) as Array<{
          role: string;
          content: unknown;
        }>;
        if (!Array.isArray(history) || history.length === 0) {
          await sendOutboundRuntime({
            channel: msg.channel,
            sessionId: msg.sessionId,
            text: "🧠 No chat history to summarize yet.",
          });
          return;
        }
        const slice = history.slice(-60);
        const formatted = slice
          .map((m, i) => {
            const c = m.content;
            const t =
              typeof c === "string"
                ? c
                : Array.isArray(c)
                  ? (c as Array<{ text?: string }>)
                      .map((p) => p?.text ?? "")
                      .filter(Boolean)
                      .join(" ")
                  : "";
            return `${i + 1}. [${m.role.toUpperCase()}] ${t.slice(0, 800)}`;
          })
          .join("\n");

        const schema = zodV4.object({
          title: zodV4.string().min(1).max(140),
          summary: zodV4.string().min(1).max(1200),
          labels: zodV4.array(zodV4.string().min(1).max(48)).max(8),
          atomic_facts: zodV4
            .array(
              zodV4.object({
                text: zodV4.string().min(2).max(500),
                kind_hint: zodV4
                  .enum([
                    "directory",
                    "command",
                    "preference",
                    "fact",
                    "code_snippet",
                    "workflow",
                    "person",
                    "project",
                    "credential_hint",
                    "favorite_app",
                    "other",
                  ])
                  .nullable(),
              })
            )
            .max(8),
        });

        // Chat-summary path uses the dedicated CHAT_SUMMARY_MODEL knob.
        // Default gpt-4.1-mini for cheap, accurate distillation.
        const model =
          env("CHAT_SUMMARY_MODEL") ??
          env("MEMORY_ENRICHMENT_MODEL") ??
          "gpt-4.1-mini";
        const { object: o } = await generateObject({
          model: textAuxModel(model),
          schema,
          system: [
            "Distill the chat into long-term memory. Output a title, a one-paragraph",
            "summary focused on decisions/conclusions/concrete details (not narration),",
            "1-6 lowercase kebab-case labels, and 0-8 atomic_facts worth remembering",
            "individually. Never copy secrets.",
          ].join("\n"),
          prompt: [
            rememberCmd.topic ? `Focus topic: ${rememberCmd.topic}\n` : "",
            `Recent conversation (${slice.length} messages):`,
            formatted,
          ]
            .filter(Boolean)
            .join("\n"),
          temperature: 0.3,
        });

        const summaryEntry = await putMemory({
          tenantId,
          kind: "chat_summary",
          title: o.title,
          content: o.summary,
          summary: o.summary,
          labels: o.labels,
          importance: 0.7,
          fields: {
            messages_considered: slice.length,
            topic: rememberCmd.topic ?? null,
            source: "remember_slash",
          },
        });

        const atomics: string[] = [];
        for (const item of o.atomic_facts) {
          try {
            const enriched = await enrichMemory({
              content: item.text,
              kindHint: item.kind_hint ?? undefined,
              context: rememberCmd.topic ? `topic: ${rememberCmd.topic}` : undefined,
            });
            const entry = await putMemory({
              tenantId,
              kind: enriched.kind,
              title: enriched.title,
              content: item.text,
              summary: enriched.summary,
              labels: enriched.labels,
              importance: enriched.importance,
              fields: enriched.fields,
            });
            atomics.push(`  • [${entry.kind}] ${entry.title}`);
          } catch {
            // skip on single-item failure
          }
        }

        await recordActivity(tenantId, {
          kind: "memory",
          summary: `chat summary saved: ${summaryEntry.title} (+${atomics.length} atomic)`,
          meta: { summary_id: summaryEntry.id, topic: rememberCmd.topic ?? null },
        });

        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: [
            `🧠 Summary saved: ${summaryEntry.title}`,
            `id: ${summaryEntry.id}`,
            `\n${o.summary.slice(0, 800)}`,
            atomics.length
              ? `\n\nAlso remembered ${atomics.length} item(s):\n${atomics.join("\n")}`
              : "",
          ]
            .filter(Boolean)
            .join("\n")
            .slice(0, 3500),
        });
      } catch (err: any) {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `❌ /remember chat failed: ${err?.message ?? String(err)}`,
        });
      }
      return;
    }

    // Single-fact path (existing behavior).
    try {
      const enriched = await enrichMemory({ content: rememberCmd.body });
      const entry = await putMemory({
        tenantId,
        kind: enriched.kind,
        title: enriched.title,
        content: rememberCmd.body,
        summary: enriched.summary,
        labels: enriched.labels,
        importance: enriched.importance,
        fields: enriched.fields,
      });
      await recordActivity(tenantId, {
        kind: "memory",
        summary: `remember: ${entry.title} (${entry.kind})`,
        meta: { id: entry.id, labels: entry.labels },
      });
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `🧠 Remembered (${entry.kind}): ${entry.title}\nid: ${entry.id}\n${entry.labels.length ? `tags: ${entry.labels.join(", ")}` : ""}`,
      });
    } catch (err: any) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❌ /remember failed: ${err?.message ?? String(err)}`,
      });
    }
    return;
  }

  // /logs or /activity — recent activity log overview.
  if (isLogsCommand(msg.text)) {
    const tenantId = tenantOf(msg);
    const entries = await listActivity(tenantId, { limit: 25 });
    if (entries.length === 0) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: "📜 No activity logged yet.",
      });
      return;
    }
    const lines = entries.map((e) => {
      const t = new Date(e.ts).toISOString().slice(11, 19);
      return `${t} [${e.kind}] ${e.summary.slice(0, 160)}`;
    });
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: `📜 Activity (${entries.length}):\n${lines.join("\n")}`.slice(0, 3500),
    });
    return;
  }

  // /debug on|off|status — toggle verbose Telegram streaming (the
  // "Thinking… / Working… / Preparing response…" placeholders + live
  // tool-call indicators). Default is OFF — bot responds with just the
  // typing indicator and a single final message.
  const debugCmd = parseDebugCommand(msg.text);
  if (debugCmd) {
    const tenantId = tenantOf(msg);
    if (debugCmd === "on") {
      await setDebugMode(tenantId, true);
      await recordAudit(tenantId, {
        kind: "settings.debug.on",
        summary: "debug mode enabled",
        before: "off",
        after: "on",
      });
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          "🐛 Debug mode ON. You'll see live status (Thinking…/Working…/tool calls) while the bot works. /debug off to hide.",
      });
    } else if (debugCmd === "off") {
      await setDebugMode(tenantId, false);
      await recordAudit(tenantId, {
        kind: "settings.debug.off",
        summary: "debug mode disabled",
        before: "on",
        after: "off",
      });
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          "✓ Debug mode OFF. You'll see only the typing indicator and the final response. /debug on to re-enable verbose mode.",
      });
    } else {
      const on = await isDebugMode(tenantId);
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `🐛 Debug mode: ${on ? "ON" : "OFF"}`,
      });
    }
    return;
  }

  // /autopilot on|off|status — opt this tenant into the proactive heartbeat.
  const autoCmd = parseAutopilotCommand(msg.text);
  if (autoCmd) {
    const tenantId = tenantOf(msg);
    if (autoCmd === "on") {
      await enableProactive(tenantId);
      await recordActivity(tenantId, {
        kind: "system",
        summary: "proactive autopilot enabled",
      });
      await recordAudit(tenantId, {
        kind: "settings.autopilot.on",
        summary: "proactive autopilot enabled",
        before: "off",
        after: "on",
      });
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          "ok, I'll start checking in. I peek every minute and ping you when there's actually something worth saying — a job lands, a task's been waiting on you, a reminder, etc. 15 min between pings, quiet at night. /autopilot off whenever.",
      });
    } else if (autoCmd === "off") {
      await disableProactive(tenantId);
      await recordActivity(tenantId, {
        kind: "system",
        summary: "proactive autopilot disabled",
      });
      await recordAudit(tenantId, {
        kind: "settings.autopilot.off",
        summary: "proactive autopilot disabled",
        before: "on",
        after: "off",
      });
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: "got it, going quiet. won't ping you proactively. /autopilot on to flip back.",
      });
    } else {
      const enabled = await isProactiveEnabled(tenantId);
      const lastMs = await getLastProactive(tenantId);
      const lastReason = await getLastProactiveReason(tenantId);
      const cd = cooldownMs();
      const lines = [
        `autopilot — ${enabled ? "on" : "off"}`,
        `cooldown: ${cd / 60000} min · quiet hours: ${isQuietHourNow() ? "yes, right now" : "no"}`,
        lastMs
          ? `last ping: ${new Date(lastMs).toISOString()}`
          : "last ping: never",
        lastReason ? `because: ${lastReason}` : "",
      ].filter(Boolean);
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: lines.join("\n"),
      });
    }
    return;
  }

  // /favorites — list favorite_app memories.
  if (isFavoritesCommand(msg.text)) {
    const tenantId = tenantOf(msg);
    const favs = await listByKind(tenantId, "favorite_app", 50);
    if (favs.length === 0) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          "⭐ No favorites yet. Use /remember <integration> is a favorite, " +
          "or the agent will mark them automatically as you use them.",
      });
      return;
    }
    const lines = favs.map(
      (f) => `• ${f.title}${f.summary ? `\n  ${f.summary}` : ""}`
    );
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: `⭐ Favorite apps (${favs.length}):\n${lines.join("\n")}`.slice(0, 3500),
    });
    return;
  }

  // /memories — show what's stored, grouped by kind.
  if (isMemoriesCommand(msg.text)) {
    const tenantId = tenantOf(msg);
    const summary = await memorySummary(tenantId, 5);
    if (summary.total === 0) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: "🧠 No memories yet. Use /remember <text> to add one.",
      });
      return;
    }
    const lines: string[] = [`🧠 Memories (${summary.total}):`];
    for (const g of summary.groups) {
      lines.push(`\n[${g.kind}]`);
      for (const e of g.entries) {
        lines.push(`• ${e.title} — id:${e.id}`);
      }
    }
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: lines.join("\n").slice(0, 3500),
    });
    return;
  }

  // /memforget <id> or /memforget tag:<tag>
  const memForget = parseMemForgetCommand(msg.text);
  if (memForget) {
    const tenantId = tenantOf(msg);
    try {
      let removed = 0;
      if (memForget.id) {
        if (await deleteMemory(tenantId, memForget.id)) removed = 1;
      } else if (memForget.tag) {
        const entries = await listByTag(tenantId, memForget.tag, 999);
        for (const e of entries) {
          if (await deleteMemory(tenantId, e.id)) removed++;
        }
      }
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `🗑️ Forgot ${removed} memory entr${removed === 1 ? "y" : "ies"}.`,
      });
    } catch (err: any) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❌ /memforget failed: ${err?.message ?? String(err)}`,
      });
    }
    return;
  }

  // /automate <description> | list | pause|resume|delete|run <id>
  const automateCmd = parseAutomateCommand(msg.text);
  if (automateCmd) {
    const tenantId = tenantOf(msg);
    const reply = (text: string) =>
      sendOutboundRuntime({ channel: msg.channel, sessionId: msg.sessionId, text });

    if (automateCmd.sub === "list") {
      const rules = await listAutomationsByTenant(tenantId);
      if (rules.length === 0) {
        await reply("🤖 No automations yet. Try: /automate when I get an email from alice@acme.com, summarize it and save to my vfs.");
        return;
      }
      const lines = rules.map((r) => {
        const flag = r.status === "error" ? "⚠️" : r.enabled ? "✅" : "⏸️";
        return `${flag} ${r.name}\n  id: ${r.id} · ${automationTriggerLabel(r.trigger)} · fired ${r.fireCount}×`;
      });
      await reply(`🤖 Automations (${rules.length}):\n${lines.join("\n")}`.slice(0, 3500));
      return;
    }

    if (automateCmd.sub === "pause" || automateCmd.sub === "resume") {
      const updated = await setAutomationEnabled(automateCmd.id, automateCmd.sub === "resume");
      if (!updated) {
        await reply(`❌ no automation with id ${automateCmd.id}`);
        return;
      }
      let extra = "";
      // Resuming a composio rule whose initial subscription failed (no
      // triggerId — e.g. the connection was expired at create time) must
      // actually (re)subscribe, or no events will ever arrive. setEnabled only
      // flips the flag, so do the subscription here.
      if (
        automateCmd.sub === "resume" &&
        updated.trigger.kind === "composio" &&
        !updated.trigger.triggerId
      ) {
        const sub = await subscribeTrigger({
          tenantId: updated.tenantId,
          slug: updated.trigger.triggerType,
        });
        if (sub.ok) {
          updated.trigger.triggerId = sub.triggerId;
          await putAutomation(updated);
          extra = `\n✅ subscribed ${updated.trigger.triggerType} (${sub.triggerId})`;
        } else {
          extra = `\n⚠️ still couldn't subscribe ${updated.trigger.triggerType}: ${sub.error}`;
        }
      }
      await reply(
        `${automateCmd.sub === "resume" ? "▶️ resumed" : "⏸️ paused"} ${updated.name} (${updated.id})${extra}`
      );
      return;
    }

    if (automateCmd.sub === "delete") {
      const rule = await getAutomation(automateCmd.id);
      if (rule?.trigger.kind === "composio" && rule.trigger.triggerId) {
        await unsubscribeTrigger(rule.trigger.triggerId).catch(() => {});
      }
      const ok = await deleteAutomation(automateCmd.id);
      await reply(ok ? `🗑️ deleted ${automateCmd.id}` : `❌ no automation with id ${automateCmd.id}`);
      return;
    }

    if (automateCmd.sub === "run") {
      const rule = await getAutomation(automateCmd.id);
      if (!rule) {
        await reply(`❌ no automation with id ${automateCmd.id}`);
        return;
      }
      const runId = await fireAutomation(automateCmd.id, "manual", { manual: true, ts: Date.now() });
      await reply(
        runId
          ? `🚀 running "${rule.name}" now — run ${runId}. I'll report back when it lands.`
          : `❌ couldn't start ${automateCmd.id} (is it enabled?)`
      );
      return;
    }

    if (automateCmd.sub !== "create") return;
    await reply("🤖 compiling your automation…");
    try {
      let compiled = await compileAutomationStep({ spec: automateCmd.spec });
      // One retry on a sharper model if the compile produced an empty action.
      if (compiled.action.mode === "job" && !compiled.action.instruction.trim()) {
        compiled = await compileAutomationStep({ spec: automateCmd.spec, retry: true });
      }
      const baseUrl =
        env("APP_BASE_URL") ??
        (process.env.VERCEL_PROJECT_PRODUCTION_URL
          ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`
          : "https://agentos-claw.vercel.app");
      const { rule, note } = await registerAutomation({
        tenantId,
        channel: msg.channel,
        sessionId: msg.sessionId,
        spec: automateCmd.spec,
        compiled,
        baseUrl,
        ...(compiled.triggerConfig ? { triggerConfig: compiled.triggerConfig } : {}),
      });
      await recordActivity(tenantId, {
        kind: "automation",
        summary: `created automation: ${rule.name}`,
        meta: { automationId: rule.id, trigger: rule.trigger.kind },
      });
      const actionDesc =
        rule.action.mode === "light"
          ? `light (${rule.action.steps.length} step${rule.action.steps.length === 1 ? "" : "s"})`
          : rule.action.mode === "plan"
            ? `plan (${rule.action.steps.length} step${rule.action.steps.length === 1 ? "" : "s"})`
            : rule.action.mode === "workforce"
              ? `workforce ${rule.action.workforceId}`
              : `${rule.action.deep ? "deep " : ""}job`;
      await reply(
        [
          `🤖 Automation created: ${rule.name}`,
          `id: ${rule.id}`,
          `trigger: ${automationTriggerLabel(rule.trigger)}`,
          `action: ${actionDesc}`,
          note ? (rule.trigger.kind === "webhook" ? `POST here to fire:\n${note}` : note) : "",
          `\nManage: /automate pause ${rule.id} · /automate run ${rule.id} · /automate delete ${rule.id}`,
        ]
          .filter(Boolean)
          .join("\n")
      );
    } catch (err: any) {
      await reply(`❌ /automate failed: ${String(err?.message ?? err).slice(0, 300)}`);
    }
    return;
  }

  // /agent create|list|delete|bind, /agents
  const agentCmd = parseAgentCommand(msg.text);
  if (agentCmd) {
    const tenantId = tenantOf(msg);
    const reply = (text: string) =>
      sendOutboundRuntime({ channel: msg.channel, sessionId: msg.sessionId, text });

    try {
      if (agentCmd.sub === "list") {
        const agents = await listAgentsByTenant(tenantId);
        if (!agents.length) {
          await reply("🧑‍💼 No sub-agents yet. Try: /agent create Scout | toolkits: gmail | persona: triages my inbox.");
          return;
        }
        const bots = await listAgentBotsByTenant(tenantId);
        const botByAgent = new Map(bots.map((b) => [b.agentId, b]));
        const lines = agents.map((a) => {
          const bot = botByAgent.get(a.id);
          return `${a.emoji} ${a.name}\n  id: ${a.id} · toolkits: ${a.toolkits.join(", ") || "none"}${bot ? ` · bot: @${bot.username}` : ""}`;
        });
        await reply(`🧑‍💼 Sub-agents (${agents.length}):\n${lines.join("\n")}`.slice(0, 3500));
        return;
      }

      if (agentCmd.sub === "delete") {
        const agent = await getSubAgent(agentCmd.id);
        if (!agent || agent.tenantId !== tenantId) {
          await reply(`❌ no agent with id ${agentCmd.id}`);
          return;
        }
        if (agent.telegramBotId) {
          const bot = await getAgentBot(agent.telegramBotId);
          if (bot) {
            await fetch(`https://api.telegram.org/bot${bot.token}/deleteWebhook`).catch(() => {});
            await deleteAgentBot(bot.botId);
          }
        }
        await deleteSubAgent(agent.id);
        await reply(`🗑️ deleted agent ${agent.emoji} ${agent.name} (${agent.id})`);
        return;
      }

      if (agentCmd.sub === "bind") {
        const agent = await getSubAgent(agentCmd.id);
        if (!agent || agent.tenantId !== tenantId) {
          await reply(`❌ no agent with id ${agentCmd.id}`);
          return;
        }
        const me = (await (
          await fetch(`https://api.telegram.org/bot${agentCmd.token}/getMe`)
        ).json()) as { ok?: boolean; result?: { username?: string } };
        if (!me?.ok) {
          await reply("❌ that bot token didn't validate with Telegram (getMe failed). Create a bot with @BotFather and paste its token.");
          return;
        }
        const username = me.result?.username ?? "unknown";
        const botId = "bot_" + randomUUID().replace(/-/g, "").slice(0, 12);
        const secret = randomUUID().replace(/-/g, "");
        const baseUrl =
          env("APP_BASE_URL") ??
          (process.env.VERCEL_PROJECT_PRODUCTION_URL
            ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`
            : "https://agentos-claw.vercel.app");
        const hookUrl = `${baseUrl}/api/claw?op=agent_telegram&bot=${botId}`;
        const sw = (await (
          await fetch(`https://api.telegram.org/bot${agentCmd.token}/setWebhook`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              url: hookUrl,
              secret_token: secret,
              allowed_updates: ["message"],
            }),
          })
        ).json()) as { ok?: boolean; description?: string };
        if (!sw?.ok) {
          await reply(`❌ setWebhook failed: ${sw?.description ?? "unknown error"}`);
          return;
        }
        await putAgentBot({ botId, tenantId, agentId: agent.id, token: agentCmd.token, secret, username });
        await putSubAgent({ ...agent, telegramBotId: botId });
        await recordActivity(tenantId, {
          kind: "automation",
          summary: `bound bot @${username} to agent ${agent.name}`,
          meta: { agentId: agent.id, botId },
        });
        await reply(
          `🤖 ${agent.emoji} ${agent.name} is now live as @${username} — message that bot directly to talk to this agent (scoped to: ${agent.toolkits.join(", ") || "no toolkits"}).`
        );
        return;
      }

      if (agentCmd.sub === "optimize") {
        const agent = await getSubAgent(agentCmd.id);
        if (!agent || agent.tenantId !== tenantId) {
          await reply(`❌ no agent with id ${agentCmd.id}`);
          return;
        }
        await launchAgentOptimization(agent.id);
        await reply(
          `🧪 Optimizing ${agent.emoji} ${agent.name} — proposing a persona tweak, A/B-testing it against the current baseline, and promoting it only if it beats baseline by the margin. I'll keep the result on the agent; check /agents to see the live persona.`
        );
        return;
      }

      // create
      const structured = parseStructuredAgentSpec(agentCmd.spec);
      let createdAgents: SubAgent[] = [];
      if (structured) {
        createdAgents = [await putSubAgent({ tenantId, ...structured })];
      } else {
        // Pure NL: reuse the workforce compiler just for its agent definitions.
        const existing = await listAgentsByTenant(tenantId);
        const compiled = await compileWorkforceStep({
          spec: `Define the sub-agent(s) described below (no team workflow needed — a single stage with the agent(s) is fine):\n\n${agentCmd.spec}`,
          existingAgents: existing.map((a) => ({ name: a.name, toolkits: a.toolkits })),
        });
        const taken = new Set(existing.map((a) => a.name.toLowerCase()));
        for (const ca of compiled.agents) {
          if (taken.has(ca.name.toLowerCase())) continue;
          createdAgents.push(
            await putSubAgent({
              tenantId,
              name: ca.name,
              emoji: ca.emoji,
              persona: ca.persona,
              toolkits: ca.toolkits,
            })
          );
        }
        if (!createdAgents.length) {
          await reply("❌ couldn't derive a new agent from that description (names may already be taken). Try the structured form: /agent create Name | toolkits: gmail | persona: ...");
          return;
        }
      }
      await reply(
        createdAgents
          .map(
            (a) =>
              `🧑‍💼 Agent created: ${a.emoji} ${a.name}\nid: ${a.id}\ntoolkits: ${a.toolkits.join(", ") || "none"}\npersona: ${a.persona.slice(0, 200)}\n\nBind a Telegram bot: /agent bind ${a.id} <botToken>`
          )
          .join("\n\n")
      );
    } catch (err: any) {
      await reply(`❌ /agent failed: ${String(err?.message ?? err).slice(0, 300)}`);
    }
    return;
  }

  // /team create|run|pause|resume|delete, /teams
  const wsCmd = parseWorkspaceCommand(msg.text);
  if (wsCmd) {
    const reply = (text: string) =>
      sendOutboundRuntime({ channel: msg.channel, sessionId: msg.sessionId, text });
    const userTenant = makeIdentity(msg.channel, msg.senderId);
    const groupChatId = telegramGroupChatId(msg);
    // The team bound to THIS group chat — most subcommands act on it.
    const boundTeam = groupChatId ? await getTeamByGroupChat(groupChatId) : null;

    try {
      if (wsCmd.sub === "help") {
        await reply(
          [
            "🤝 Workspaces — a shared team namespace: one set of connected accounts, automations, triggers, /job + deep jobs, /code projects, files and memory, shared by everyone in the group.",
            "",
            "Setup (create a Telegram group, add me, then run these IN the group):",
            "• /workspace create <name> — turn this group into a shared team",
            "• /workspace invite <@user|user_id> — I DM them a join link (or hand you a shareable one)",
            "• /workspace link — get a shareable join link",
            "• /workspace members — who's on the team",
            "• /workspace rename <name> · /workspace leave · /workspace delete",
            "• /workspace list — teams you belong to",
            "• /workspace rebind <teamId> — re-attach a team after Telegram upgrades the group (chat id changes)",
            boundTeam ? `\nThis group is team "${boundTeam.name}" (${boundTeam.id}).` : "",
          ]
            .filter(Boolean)
            .join("\n")
        );
        return;
      }

      if (wsCmd.sub === "list") {
        const teams = await listTeamsForUser(userTenant);
        if (!teams.length) {
          await reply(
            "🤝 You're not on any teams yet. Create one: make a Telegram group, add me, then /workspace create <name> inside it."
          );
          return;
        }
        await reply(
          `🤝 Your teams (${teams.length}):\n` +
            teams
              .map(
                (t) =>
                  `• ${t.name} (${t.id}) — ${t.members.length} member(s)${
                    t.tgGroupChatId ? "" : " · no group bound"
                  }`
              )
              .join("\n")
        );
        return;
      }

      if (wsCmd.sub === "create") {
        if (!groupChatId) {
          await reply(
            "🤝 Run /workspace create <name> INSIDE a Telegram group: create a group, add me as a member (make me admin for invite links), then run it there. That group becomes the team's shared chat."
          );
          return;
        }
        if (boundTeam) {
          await reply(
            `🤝 This group is already team "${boundTeam.name}" (${boundTeam.id}). Use /workspace invite to add people.`
          );
          return;
        }
        const team = await createTeam({
          name: wsCmd.name,
          channel: "telegram",
          ownerTenantId: userTenant,
          ownerSenderId: msg.senderId,
          ownerUsername: msg.senderUsername,
          tgGroupChatId: groupChatId,
        });
        const { link, kind } = await buildTeamInviteLink(team);
        await reply(
          [
            `✅ Created team "${team.name}" (${team.id}).`,
            "Everyone in this group now shares one workspace — connected accounts, automations, jobs, /code projects, files and memory are all shared here.",
            "",
            kind === "group"
              ? `Invite others (or /workspace invite <@user|id>):\n${link}`
              : `Invite link:\n${link}\n(Tip: make me a group admin so I can generate real Telegram invite links.)`,
          ].join("\n")
        );
        return;
      }

      if (wsCmd.sub === "rebind") {
        // Recovery for Telegram's group→supergroup migration (chat id changes
        // silently, orphaning the team). Owner runs this in the NEW group.
        if (!groupChatId) {
          await reply("🤝 Run /workspace rebind <teamId> inside the group you want to attach.");
          return;
        }
        const team = await getTeam(wsCmd.teamId);
        if (!team || team.ownerTenantId !== userTenant) {
          await reply(`❌ No team ${wsCmd.teamId} that you own.`);
          return;
        }
        if (boundTeam && boundTeam.id !== team.id) {
          await reply(`❌ This group is already bound to "${boundTeam.name}" (${boundTeam.id}).`);
          return;
        }
        await bindGroupChat(team.id, groupChatId);
        await reply(`🔗 Rebound team "${team.name}" to this group. Shared workspace restored.`);
        return;
      }

      // The remaining subcommands operate on the team bound to this chat.
      if (!boundTeam) {
        await reply(
          "🤝 Run this inside your team's Telegram group. No team is bound to this chat yet — /workspace create <name> to make one."
        );
        return;
      }

      if (wsCmd.sub === "link") {
        const { link, kind } = await buildTeamInviteLink(boundTeam);
        await reply(
          kind === "group"
            ? `🔗 Join link for "${boundTeam.name}":\n${link}`
            : `🔗 Join link for "${boundTeam.name}":\n${link}\n(Make me a group admin for a proper Telegram invite link.)`
        );
        return;
      }

      if (wsCmd.sub === "members") {
        await reply(
          `🤝 ${boundTeam.name} — ${boundTeam.members.length} member(s):\n` +
            boundTeam.members
              .map(
                (m) =>
                  `• ${m.username ? "@" + m.username : m.senderId}${
                    m.role === "owner" ? " (owner)" : ""
                  }`
              )
              .join("\n")
        );
        return;
      }

      if (wsCmd.sub === "rename") {
        if (boundTeam.ownerTenantId !== userTenant) {
          await reply("❌ Only the team owner can rename it.");
          return;
        }
        const t = await renameTeam(boundTeam.id, wsCmd.name);
        await reply(t ? `✏️ Renamed to "${t.name}".` : "❌ rename failed");
        return;
      }

      if (wsCmd.sub === "leave") {
        if (boundTeam.ownerTenantId === userTenant) {
          await reply("❌ You're the owner — use /workspace delete to remove the team instead.");
          return;
        }
        await removeTeamMember(boundTeam.id, userTenant);
        await reply(
          `👋 You've left "${boundTeam.name}". Your messages in this group will no longer use the shared workspace.`
        );
        return;
      }

      if (wsCmd.sub === "delete") {
        if (boundTeam.ownerTenantId !== userTenant) {
          await reply("❌ Only the team owner can delete it.");
          return;
        }
        await deleteTeam(boundTeam.id);
        await reply(
          `🗑️ Deleted team "${boundTeam.name}". This group reverts to per-user (no shared workspace).`
        );
        return;
      }

      if (wsCmd.sub === "invite") {
        const { link } = await buildTeamInviteLink(boundTeam);
        const targetSession = await resolveInviteTargetSession(wsCmd.who);
        const inviteText =
          `🤝 You've been invited to the "${boundTeam.name}" team. Tap to join the shared group chat — that's where the shared bot, accounts and automations live:\n${link}`;
        if (targetSession) {
          try {
            await sendOutboundRuntime({
              channel: "telegram",
              sessionId: targetSession,
              text: inviteText,
            });
            await reply(`📨 Invited ${wsCmd.who} — I DMed them a join link.`);
            return;
          } catch {
            // couldn't DM (hasn't started the bot) — fall through to shareable link
          }
        }
        await reply(
          [
            targetSession
              ? `⚠️ I couldn't DM ${wsCmd.who} (they may not have messaged me yet). Share this link with them:`
              : `I can only DM people who've already started me. Share this link with ${wsCmd.who}:`,
            link,
          ].join("\n")
        );
        return;
      }
    } catch (err: any) {
      await reply(`❌ /workspace ${wsCmd.sub} failed: ${err?.message ?? String(err)}`);
    }
    return;
  }

  const teamCmd = parseTeamCommand(msg.text);
  if (teamCmd) {
    const tenantId = tenantOf(msg);
    const reply = (text: string) =>
      sendOutboundRuntime({ channel: msg.channel, sessionId: msg.sessionId, text });

    try {
      if (teamCmd.sub === "list") {
        const teams = await listWorkforcesByTenant(tenantId);
        if (!teams.length) {
          await reply("🤝 No teams yet. Try: /team create every weekday at 9am, a Scout agent (gmail) triages my inbox, then a Writer agent drafts replies.");
          return;
        }
        const lines: string[] = [];
        for (const t of teams) {
          const rule = await getAutomation(t.automationId);
          const flag = rule ? (rule.status === "error" ? "⚠️" : rule.enabled ? "✅" : "⏸️") : "❓";
          lines.push(
            `${flag} ${t.emoji ?? "🤝"} ${t.name}\n  id: ${t.id} · ${rule ? automationTriggerLabel(rule.trigger) : "no trigger"} · ${t.stages.length} stage(s)`
          );
        }
        await reply(`🤝 Teams (${teams.length}):\n${lines.join("\n")}`.slice(0, 3500));
        return;
      }

      if (teamCmd.sub === "run" || teamCmd.sub === "pause" || teamCmd.sub === "resume" || teamCmd.sub === "delete") {
        const team = await getWorkforce(teamCmd.id);
        if (!team || team.tenantId !== tenantId) {
          await reply(`❌ no team with id ${teamCmd.id}`);
          return;
        }
        if (teamCmd.sub === "run") {
          const runId = await fireAutomation(team.automationId, "manual", { manual: true, ts: Date.now() });
          await reply(
            runId
              ? `🚀 running team "${team.name}" now — run ${runId}. I'll report the composed summary when all stages land.`
              : `❌ couldn't start ${team.id} (is its trigger rule enabled?)`
          );
          return;
        }
        if (teamCmd.sub === "delete") {
          const rule = await getAutomation(team.automationId);
          if (rule?.trigger.kind === "composio" && rule.trigger.triggerId) {
            await unsubscribeTrigger(rule.trigger.triggerId).catch(() => {});
          }
          await deleteAutomation(team.automationId).catch(() => {});
          await deleteWorkforce(team.id);
          await reply(`🗑️ deleted team ${team.name} (${team.id}) and its trigger`);
          return;
        }
        const enabled = teamCmd.sub === "resume";
        await setAutomationEnabled(team.automationId, enabled);
        await putWorkforce({ ...team, enabled });
        await reply(`${enabled ? "▶️ resumed" : "⏸️ paused"} team ${team.name} (${team.id})`);
        return;
      }

      if (teamCmd.sub !== "create") {
        await reply("❌ unknown /team subcommand");
        return;
      }
      const teamSpec = teamCmd.spec;

      await reply("🤝 compiling your team…");
      const baseUrl =
        env("APP_BASE_URL") ??
        (process.env.VERCEL_PROJECT_PRODUCTION_URL
          ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL}`
          : "https://agentos-claw.vercel.app");
      const { team, rule, note, newAgents, members } = await createWorkforceFromSpec({
        tenantId,
        channel: msg.channel,
        sessionId: msg.sessionId,
        spec: teamSpec,
        baseUrl,
      });

      const nameOf = (id: string) => {
        const a = members.find((m) => m.id === id);
        return a ? `${a.emoji} ${a.name}` : id;
      };
      await reply(
        [
          `🤝 Team created: ${team.emoji ?? ""} ${team.name}`.trim(),
          `id: ${team.id}`,
          `trigger: ${automationTriggerLabel(rule.trigger)}`,
          newAgents.length
            ? `new agents: ${newAgents.map((a) => `${a.emoji} ${a.name} (${a.id})`).join(", ")}`
            : "",
          `stages:\n${describeStages(team.stages, nameOf)}`,
          note ? (rule.trigger.kind === "webhook" ? `POST here to fire:\n${note}` : note) : "",
          `\nManage: /team run ${team.id} · /team pause ${team.id} · /team delete ${team.id}`,
        ]
          .filter(Boolean)
          .join("\n")
      );
    } catch (err: any) {
      await reply(`❌ /team failed: ${String(err?.message ?? err).slice(0, 300)}`);
    }
    return;
  }

  // /subscribe <SLUG> [{json config}]
  const subCmd = parseSubscribeCommand(msg.text);
  if (subCmd) {
    const tenantId = tenantOf(msg);
    let triggerConfig: Record<string, unknown> | undefined;
    if (subCmd.configJson) {
      try {
        triggerConfig = JSON.parse(subCmd.configJson) as Record<string, unknown>;
      } catch (err: any) {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `❌ /subscribe: bad JSON config — ${err?.message ?? "parse error"}`,
        });
        return;
      }
    }
    const out = await subscribeTrigger({
      tenantId,
      slug: subCmd.slug,
      triggerConfig,
    });
    if (out.ok) {
      await recordActivity(tenantId, {
        kind: "trigger",
        summary: `subscribed: ${subCmd.slug} → ${out.triggerId}`,
        meta: { triggerId: out.triggerId, slug: subCmd.slug, config: triggerConfig },
      });
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `🔔 Subscribed to ${subCmd.slug}\nid: ${out.triggerId}\nUse /unsubscribe ${out.triggerId} to stop.`,
      });
    } else {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❌ /subscribe failed: ${out.error}`,
      });
    }
    return;
  }

  // /triggers — list active subscriptions.
  if (isTriggersCommand(msg.text)) {
    const tenantId = tenantOf(msg);
    const subs = await listSubscriptions(tenantId);
    if (subs.length === 0) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: "🔔 No active triggers. Use /subscribe <SLUG> to add one.",
      });
      return;
    }
    const lines = subs.map(
      (s) => `• ${s.triggerName}\n  id: ${s.triggerId}`
    );
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: `🔔 Active triggers (${subs.length}):\n${lines.join("\n")}`.slice(0, 3500),
    });
    return;
  }

  // /unsubscribe <trigger_id>
  const unsubId = parseUnsubscribeCommand(msg.text);
  if (unsubId) {
    const tenantId = tenantOf(msg);
    const out = await unsubscribeTrigger(unsubId);
    if (out.ok) {
      await recordActivity(tenantId, {
        kind: "trigger",
        summary: `unsubscribed ${unsubId}`,
        meta: { triggerId: unsubId },
      });
    }
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text: out.ok
        ? `🔕 Unsubscribed ${unsubId}`
        : `❌ /unsubscribe failed: ${out.error ?? "unknown"}`,
    });
    return;
  }

  // /code … — long-running coding projects. Dispatch is fire-and-forget;
  // the workflow runs asynchronously and writes progress to the project
  // store. Status is queryable via /code status PROJECT_ID and /code list.
  const codeCmd = parseClaudeCodeCommand(msg.text);
  if (codeCmd) {
    const tenantId = tenantOf(msg);

    if (codeCmd.sub === "list") {
      const [active, recent] = await Promise.all([
        listActiveCodeProjects(tenantId),
        listRecentCodeProjects(tenantId, 10),
      ]);
      const activeSet = new Set(active);
      const rows: string[] = [];
      for (const pid of recent) {
        const p = await getCodeProject(pid);
        if (!p) continue;
        const tag = activeSet.has(pid) ? "🟢" : p.status === "failed" ? "❌" : "·";
        rows.push(`${tag} ${pid} — ${p.title.slice(0, 80)} (${p.status})`);
      }
      const text = rows.length
        ? `your code projects:\n` + rows.join("\n")
        : `no code projects yet — /code <task> kicks one off.`;
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text,
      });
      return;
    }

    if (codeCmd.sub === "status") {
      const proj = await getCodeProject(codeCmd.projectId);
      if (!proj || proj.tenantId !== tenantId) {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `❓ No such code project: ${codeCmd.projectId}`,
        });
        return;
      }
      const recent = await getCodeThoughts(codeCmd.projectId, { limit: 8 });
      const tasks = await getCodeTasks(codeCmd.projectId, 1);
      const lines = recent
        .slice()
        .reverse()
        .map((t) => `• [${t.kind}] ${t.text.slice(0, 200)}`)
        .join("\n");
      const lastTask = tasks[0];
      const lastOut =
        proj.lastOutput && proj.lastOutput.length > 0
          ? `\n\nLast output:\n${proj.lastOutput.slice(0, 1800)}`
          : "";
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          `${codeCmd.projectId} — ${proj.status} · ${proj.engine} · turn ${proj.turnCount}\n` +
          `${proj.title}` +
          (lastTask ? `\n\nlast task: ${lastTask.task.slice(0, 200)}` : "") +
          (lines ? `\n\n${lines}` : `\n\n(nothing logged yet)`) +
          lastOut,
      });
      return;
    }

    if (codeCmd.sub === "push") {
      const proj = await getCodeProject(codeCmd.projectId);
      if (!proj || proj.tenantId !== tenantId) {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `❓ No such code project: ${codeCmd.projectId}`,
        });
        return;
      }
      const repoUrl = codeCmd.repoUrl ?? proj.repoUrl;
      if (!repoUrl) {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text:
            `❌ /code push needs a repo URL. ` +
            `Usage: /code push ${codeCmd.projectId} https://github.com/me/repo [branch]`,
        });
        return;
      }
      const branch =
        codeCmd.branch ?? `agentos/${codeCmd.projectId}-${Date.now().toString(36)}`;
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `📤 Pushing ${codeCmd.projectId} → ${repoUrl} (branch ${branch})…`,
      });
      try {
        // Push runs the engine with a no-op task in the existing workdir,
        // then commits + pushes. Reuses runClaudeCode's repo-push pipeline.
        const result = await runClaudeCode({
          prompt:
            "The user has asked to materialize this project's work to GitHub. " +
            "Don't make further changes; just summarize what's in the workdir in one paragraph.",
          tenantId,
          continueSession: true,
          absoluteWorkdir: proj.sandboxWorkdir,
          forceEngine: proj.engine,
          repoUrl,
          baseBranch: proj.baseBranch,
          pushToBranch: branch,
          timeoutMs: 8 * 60 * 1000,
        });
        if (result.repoPush?.ok) {
          await updateCodeProject(codeCmd.projectId, {
            repoUrl,
            pushedBranch: branch,
            status: "done",
          });
          await recordAudit(tenantId, {
            kind: "tool.code_push",
            summary: `/code push ${codeCmd.projectId} → ${repoUrl} (${branch})`,
            meta: {
              projectId: codeCmd.projectId,
              repoUrl,
              branch,
              ok: true,
            },
          });
          await sendOutboundRuntime({
            channel: msg.channel,
            sessionId: msg.sessionId,
            text: `✅ Pushed ${codeCmd.projectId} to ${repoUrl} branch \`${branch}\``,
          });
        } else {
          await recordAudit(tenantId, {
            kind: "tool.code_push",
            summary: `/code push ${codeCmd.projectId} failed: ${(result.repoPush?.error ?? result.error ?? "unknown").slice(0, 120)}`,
            meta: {
              projectId: codeCmd.projectId,
              repoUrl,
              branch,
              ok: false,
              error: result.repoPush?.error ?? result.error,
            },
          });
          await sendOutboundRuntime({
            channel: msg.channel,
            sessionId: msg.sessionId,
            text: `❌ Push failed: ${result.repoPush?.error ?? result.error ?? "unknown"}`,
          });
        }
      } catch (err: any) {
        await recordAudit(tenantId, {
          kind: "tool.code_push",
          summary: `/code push ${codeCmd.projectId} crashed: ${String(err?.message ?? err).slice(0, 120)}`,
          meta: {
            projectId: codeCmd.projectId,
            repoUrl,
            branch,
            ok: false,
            error: err?.message ?? String(err),
          },
        });
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `❌ /code push crashed: ${err?.message ?? String(err)}`,
        });
      }
      return;
    }

    // /code attach <projectId> <task>
    if (codeCmd.sub === "attach") {
      const proj = await getCodeProject(codeCmd.projectId);
      if (!proj || proj.tenantId !== tenantId) {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `❓ No such code project: ${codeCmd.projectId}`,
        });
        return;
      }
      await updateCodeProject(codeCmd.projectId, {
        status: "pending",
        currentTask: codeCmd.task,
      });
      await recordActivity(tenantId, {
        kind: "code",
        summary: `attach ${codeCmd.projectId}: ${codeCmd.task.slice(0, 100)}`,
        meta: { projectId: codeCmd.projectId },
      });
      await recordAudit(tenantId, {
        kind: "tool.code_dispatch",
        summary: `/code attach ${codeCmd.projectId}: ${codeCmd.task.slice(0, 120)}`,
        meta: {
          projectId: codeCmd.projectId,
          mode: "attach",
          engine: proj.engine,
        },
      });
      await start(codeWorkflow, [codeCmd.projectId, codeCmd.task]);
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text:
          `picking up ${codeCmd.projectId}. I'll come back when the turn's done — ` +
          `or /code status ${codeCmd.projectId} to peek.`,
      });
      return;
    }

    // /code <task> — new project.
    // Pick engine: explicit override > auto-detect. If neither is available,
    // fail fast with the same message the runner would emit.
    let engine: CodeEngine;
    if (codeCmd.engineOverride) {
      engine = codeCmd.engineOverride;
    } else {
      const detected = chooseEngine();
      if (detected === "none") {
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text:
            "❌ No coding engine configured. Set ANTHROPIC_API_KEY (Claude Code) " +
            "or OPENAI_API_KEY (OpenCode fallback) in Vercel env.",
        });
        return;
      }
      engine = detected;
    }

    // Sandbox + VFS layout: project-stable so claude --continue picks up
    // session history across turns and the agent can find /workspace files
    // when chatting about the project.
    const sandboxWorkdir = `/tmp/claw-browser/cc-workdirs/${tenantId.replace(/[^a-zA-Z0-9._-]/g, "_")}/projects/PROJECT_PLACEHOLDER`;
    const vfsRoot = `/workspace/claude_code/projects/PROJECT_PLACEHOLDER`;

    const proj = await createCodeProject({
      tenantId,
      channel: msg.channel,
      sessionId: msg.sessionId,
      title: codeCmd.task.slice(0, 200),
      engine,
      sandboxWorkdir: "/tmp/placeholder",
      vfsRoot: "/workspace/placeholder",
      repoUrl: codeCmd.repoUrl,
      baseBranch: codeCmd.baseBranch,
    });

    // Now patch the workdir/vfsRoot with the real project ID.
    const realSandboxWorkdir = sandboxWorkdir.replace(
      "PROJECT_PLACEHOLDER",
      proj.projectId
    );
    const realVfsRoot = vfsRoot.replace("PROJECT_PLACEHOLDER", proj.projectId);
    await updateCodeProject(proj.projectId, {
      sandboxWorkdir: realSandboxWorkdir,
      vfsRoot: realVfsRoot,
    });

    await recordActivity(tenantId, {
      kind: "code",
      summary: `new project ${proj.projectId}: ${codeCmd.task.slice(0, 100)}`,
      meta: {
        projectId: proj.projectId,
        engine,
        repoUrl: codeCmd.repoUrl,
      },
    });
    await recordAudit(tenantId, {
      kind: "tool.code_dispatch",
      summary: `/code new ${proj.projectId} (${engine}): ${codeCmd.task.slice(0, 120)}`,
      meta: {
        projectId: proj.projectId,
        mode: "new",
        engine,
        repoUrl: codeCmd.repoUrl,
      },
    });
    await start(codeWorkflow, [proj.projectId, codeCmd.task]);
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text:
        `${engine === "claude" ? "claude" : "opencode"} on it. project ${proj.projectId} — ` +
        `going to be a few minutes. I'll surface back when the turn's done. ` +
        `/code status ${proj.projectId} for a peek, /code attach ${proj.projectId} <followup> to keep going.`,
    });
    return;
  }

  // /logins — list hostnames the user has saved browser sessions for.
  if (isLoginsCommand(msg.text)) {
    const tenantId = tenantOf(msg);
    try {
      const domains = await listCookieDomains(tenantId);
      const text = domains.length
        ? `🔐 Saved logins (${domains.length}):\n` +
          domains.map((d) => `• ${d}`).join("\n") +
          `\n\nUse /forget <hostname> or /forget all to remove.`
        : `🔐 No saved logins. Ask me to log in to a site to save one.`;
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text,
      });
    } catch (err: any) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❌ /logins failed: ${err?.message ?? String(err)}`,
      });
    }
    return;
  }

  // /forget <hostname> | /forget all — delete saved sessions.
  const forgetCmd = parseForgetCommand(msg.text);
  if (forgetCmd) {
    const tenantId = tenantOf(msg);
    try {
      if (forgetCmd.target.toLowerCase() === "all") {
        await forgetAll(tenantId);
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `🗑️ All saved logins forgotten.`,
        });
      } else {
        const removed = await forgetHostname(tenantId, forgetCmd.target);
        await sendOutboundRuntime({
          channel: msg.channel,
          sessionId: msg.sessionId,
          text: `🗑️ Forgot ${removed} cookies for ${forgetCmd.target}.`,
        });
      }
    } catch (err: any) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❌ /forget failed: ${err?.message ?? String(err)}`,
      });
    }
    return;
  }

  // /ask <jobId> <question> — read-only side-channel; does not touch actor.
  const askCmd = parseAskCommand(msg.text);
  if (askCmd) {
    try {
      const result = await askJob({
        jobId: askCmd.jobId,
        question: askCmd.question,
      });
      const text =
        "ok" in result && result.ok
          ? `🧵 ${result.jobId} — ${result.status}\n${result.answer}`
          : `❓ No such job: ${askCmd.jobId}`;
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text,
      });
    } catch (err: any) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❌ /ask failed: ${err?.message ?? String(err)}`,
      });
    }
    return;
  }

  // /status <jobId> — inline progress check.
  const statusJobId = parseStatusCommand(msg.text);
  if (statusJobId) {
    const meta = await getJobMeta(statusJobId);
    if (!meta) {
      await sendOutboundRuntime({
        channel: msg.channel,
        sessionId: msg.sessionId,
        text: `❓ No such job: ${statusJobId}`,
      });
      return;
    }
    const thoughts = await getThoughts(statusJobId, { limit: 6 });
    const lines = thoughts
      .slice()
      .reverse()
      .map((t) => `• [${t.kind}] ${t.text}`)
      .join("\n");
    const cost =
      typeof meta.estimatedCost === "number"
        ? ` — $${meta.estimatedCost.toFixed(3)}`
        : "";
    await sendOutboundRuntime({
      channel: msg.channel,
      sessionId: msg.sessionId,
      text:
        `${statusJobId} — ${meta.status}${cost}\n` +
        (lines ? `${lines}` : `(nothing logged yet)`),
    });
    return;
  }

  // Fire any chat-pattern automations whose regex matches this message. This is
  // additive and runs for EVERY conversational message — explicit user rules
  // fire even when the group-etiquette gate below keeps the bot quiet. Fire-
  // and-forget so a slow automation never delays the reply.
  fireMatchingChatAutomations(msg).catch(() => {});

  // Team-group etiquette: in a shared workspace group, people mostly talk to
  // each other. Only reply when the bot is ADDRESSED (reply-gesture, @mention,
  // or called by name) or when the cheap chime-in gate says it can add clear
  // value. Otherwise record the message into history (speaker-labeled) so the
  // bot has full context the next time it IS addressed, and stay silent.
  if (teamTenant) {
    const addressed = await isAddressedToBot(msg);
    if (!addressed && !(await shouldChimeIn(msg))) {
      await recordSilentGroupMessage(msg);
      return;
    }
  }

  // Conversational message the bot will answer. React to it with an emoji
  // when there's a genuine hook — makes the bot feel like a real person
  // texting back. Fire-and-forget: never blocks or delays the actual reply,
  // and any failure is swallowed inside maybeReactToMessage.
  maybeReactToMessage(msg);

  await routeToSession(msg);
}

// Best-effort emoji reaction to a user's Telegram message. Telegram-only
// (other channels have no reaction concept). Runs the lightweight classifier
// and, if it returns an emoji, sets the reaction via the Bot API. Scheduled
// on waitUntil so the conversation proceeds immediately.
function maybeReactToMessage(msg: InboundMessage): void {
  if (msg.channel !== "telegram") return;
  const raw = msg.raw as any;
  const message =
    raw?.message ?? raw?.edited_message ?? raw?.channel_post ?? undefined;
  const messageId =
    typeof message?.message_id === "number" ? message.message_id : null;
  if (!messageId) return;

  waitUntil(
    (async () => {
      try {
        const emoji = await pickReactionEmoji(msg.text ?? "");
        if (!emoji) return;
        await telegramSetMessageReaction(msg.sessionId, messageId, emoji);
      } catch {
        // reactions are pure delight — never let one break anything
      }
    })()
  );
}

// ============================================================
// GET handler
// ============================================================
export async function GET(req: Request) {
  const url = new URL(req.url);
  const op = url.searchParams.get("op");

  if (op === "health") return jsonOk({ ts: Date.now() });

  // Recent Composio webhook deliveries — observability for the trigger
  // notification pipeline (was this event received? delivered? why not?).
  if (op === "webhooklog") {
    const { getRecentWebhookHits } = await import("@/app/lib/composioWebhook");
    const hits = await getRecentWebhookHits(25);
    return jsonOk({ count: hits.length, hits });
  }

  // Read-only job inspector — meta + recent thoughts for any jobId. Useful
  // observability for deep jobs (watch the orchestrator + depth reviewer).
  if (op === "jobthoughts") {
    const id = url.searchParams.get("id") ?? "";
    if (!id) return jsonOk({ error: "missing id" });
    const meta = await getJobMeta(id);
    const thoughts = await getThoughts(id, { limit: 60 });
    return jsonOk({
      meta: meta
        ? {
            jobId: meta.jobId,
            status: meta.status,
            kind: meta.kind,
            escalated: meta.escalated ?? false,
            depthPasses: meta.depthPasses ?? 0,
            estimatedCost: meta.estimatedCost ?? 0,
            resultLen: meta.resultText?.length ?? 0,
          }
        : null,
      thoughts: thoughts
        .slice()
        .reverse()
        .map((t) => ({ kind: t.kind, text: t.text })),
    });
  }

  if (op === "debug_ctrig") {
    const {
      listAllCustomSubscriptions,
      debugPollSub,
      debugActionSchema,
      debugRunAction,
      unsubscribeCustomTrigger,
    } = await import("@/app/lib/customTriggers");
    const { listConnectedToolkits } = await import(
      "@/app/lib/composioConnections"
    );
    const action = url.searchParams.get("action") ?? "list";
    if (action === "poll") {
      const sub = url.searchParams.get("sub") ?? "";
      return jsonOk(await debugPollSub(sub));
    }
    if (action === "schema") {
      const tenantId = url.searchParams.get("tenant") ?? "";
      const slug = url.searchParams.get("slug") ?? "";
      return jsonOk(await debugActionSchema(tenantId, slug));
    }
    if (action === "accounts") {
      const tenantId = url.searchParams.get("tenant") ?? "";
      return jsonOk({ accounts: await listConnectedToolkits(tenantId) });
    }
    if (action === "run") {
      const tenantId = url.searchParams.get("tenant") ?? "";
      const slug = url.searchParams.get("slug") ?? "";
      const cai = url.searchParams.get("cai") ?? undefined;
      let runArgs: Record<string, unknown> = {};
      try {
        runArgs = JSON.parse(url.searchParams.get("args") ?? "{}");
      } catch {
        /* ignore */
      }
      return jsonOk(await debugRunAction(tenantId, slug, runArgs, cai));
    }
    if (action === "unsub") {
      const sub = url.searchParams.get("sub") ?? "";
      await unsubscribeCustomTrigger(sub);
      return jsonOk({ unsubscribed: sub });
    }
    return jsonOk({ subs: await listAllCustomSubscriptions() });
  }

  if (op === "debug_uitenant") {
    const { resolveUiTenant } = await import("@/app/lib/uiTenant");
    const { env: readEnv } = await import("@/app/lib/env");
    return jsonOk({
      resolved: await resolveUiTenant(null),
      adminIdentities: readEnv("ADMIN_IDENTITIES") ?? null,
      uiDefaultTenant: readEnv("UI_DEFAULT_TENANT") ?? null,
    });
  }

  if (op === "debug_diag") {
    const tenant = url.searchParams.get("tenant") ?? "";
    const { listActivity, countActivity } = await import("@/app/lib/activityLog");
    const { getSessionMeta, getLastSession } = await import("@/app/lib/sessionMeta");
    const { getStore: gs } = await import("@/app/lib/store");
    const store = gs();
    const colon = tenant.indexOf(":");
    const channel = colon > 0 ? tenant.slice(0, colon) : "telegram";
    const [count, recent, metaExact, metaSender, lastForChannel, lastRaw] =
      await Promise.all([
        countActivity(tenant).catch(() => -1),
        listActivity(tenant, { limit: 15 }).catch(() => []),
        getSessionMeta(tenant).catch(() => null),
        getSessionMeta(colon > 0 ? tenant.slice(colon + 1) : tenant).catch(() => null),
        getLastSession(channel as any).catch(() => null),
        store.get(`last:${channel}`).catch(() => null),
      ]);
    return jsonOk({
      tenant,
      activityCount: count,
      recent: recent.map((e: any) => ({ ts: e.ts, kind: e.kind, summary: e.summary })),
      delivery: {
        // deliverToTenant looks up sess:meta:{tenant} first, else last:{channel}
        sessMetaExactKey: `sess:meta:${tenant}`,
        sessMetaExact: metaExact,
        sessMetaBySenderId: metaSender,
        getLastSession: lastForChannel,
        lastChannelPointer: lastRaw,
      },
    });
  }

  if (op === "debug_connect") {
    // Mint a real, tenant-scoped OAuth reconnect link for a toolkit. Returns
    // the redirect URL only — does NOT message the user. Used to hand a
    // working link to a tenant whose in-bot connect flow handed out a generic
    // (non-functional) app.composio.dev URL.
    const { initiateConnection, listConnectedToolkits, isToolkitConnected } = await import(
      "@/app/lib/composioConnections"
    );
    const tenantId = url.searchParams.get("tenant") ?? "";
    const toolkit = url.searchParams.get("toolkit") ?? "";
    if (!tenantId) {
      return jsonOk({ error: "tenant is required" });
    }
    // No toolkit → list every connected account + status for this tenant.
    if (!toolkit) {
      const all = await listConnectedToolkits(tenantId);
      return jsonOk({ ok: true, tenantId, connected: all });
    }
    // action=check → run the exact same check the agent does (status-aware).
    if ((url.searchParams.get("action") ?? "") === "check") {
      const res = await isToolkitConnected(tenantId, toolkit);
      return jsonOk({ ok: true, tenantId, toolkit, result: res });
    }
    const res = await initiateConnection({ tenantId, toolkitSlug: toolkit });
    return jsonOk(res);
  }

  if (op === "debug_seed_state") {
    // Seed a recurring automation's durable state file so the next firing
    // reuses an already-created resource (e.g. an existing spreadsheet id)
    // instead of creating a new one. `content` is the raw JSON to store.
    const { seedAutomationState } = await import(
      "@/app/steps/automationRunSteps"
    );
    const id = url.searchParams.get("id") ?? "";
    const content = url.searchParams.get("content") ?? "";
    if (!id) return jsonOk({ error: "id is required" });
    if (!content) {
      // Read-only: dump the current state.json so we can see what the agent
      // reads each run (and whether a stale lastMessageId is misleading it).
      const { readAutomationState } = await import(
        "@/app/steps/automationRunSteps"
      );
      const cur = await readAutomationState(id);
      return jsonOk(cur ? { ok: true, id, ...cur } : { error: "automation not found" });
    }
    const res = await seedAutomationState(id, content);
    return jsonOk(res ? { ok: true, ...res, content } : { error: "automation not found" });
  }

  if (op === "debug_model") {
    // Smoke-test any model id through our routing chokepoint (OpenAI, Gemini,
    // or gateway-routed vendors like anthropic/claude-*). Verifies a model is
    // actually reachable in THIS deployment before we point a purpose at it.
    const { resolveModel, providerFor } = await import("@/app/lib/modelRouting");
    const { generateText } = await import("ai");
    const modelName = url.searchParams.get("model") ?? "";
    if (!modelName) return jsonOk({ error: "model is required" });
    const prompt =
      url.searchParams.get("prompt") ??
      "Reply with exactly: OK <your model family and version>";
    const t0 = Date.now();
    try {
      const out = await generateText({
        model: resolveModel(modelName),
        prompt,
      });
      return jsonOk({
        ok: true,
        model: modelName,
        provider: providerFor(modelName),
        ms: Date.now() - t0,
        text: out.text.slice(0, 500),
        usage: out.usage ?? null,
      });
    } catch (err: any) {
      return jsonOk({
        ok: false,
        model: modelName,
        provider: providerFor(modelName),
        ms: Date.now() - t0,
        error: String(err?.message ?? err).slice(0, 500),
      });
    }
  }

  if (op === "debug_team") {
    // Dump a workforce + its trigger rule (mission, composio filter, enabled,
    // last fire) so a "trigger didn't fire" report is diagnosable in one curl:
    //   /api/claw?op=debug_team&id=team_xxx
    const id = url.searchParams.get("id") ?? "";
    if (!id) return jsonOk({ error: "id is required" });
    const { getWorkforce } = await import("@/app/lib/agents");
    const team = await getWorkforce(id);
    if (!team) return jsonOk({ error: "team not found" });
    const rule = team.automationId ? await getAutomation(team.automationId) : null;
    return jsonOk({
      team: {
        id: team.id,
        name: team.name,
        enabled: team.enabled,
        spec: team.spec,
        stages: team.stages,
        automationId: team.automationId,
      },
      rule: rule
        ? {
            id: rule.id,
            enabled: rule.enabled,
            status: rule.status,
            trigger: rule.trigger,
            lastFiredAt: rule.lastFiredAt ?? null,
            fireCount: rule.fireCount,
            lastRunId: rule.lastRunId ?? null,
          }
        : { error: "trigger rule missing" },
    });
  }

  if (op === "debug_agent_eval") {
    // Inspect one agent's eval: its record + recent grader notes, and (with
    // run=1) a fresh verbose pass showing the synthesized probe task and the
    // exact output the grader saw — so a low score is diagnosable.
    //   /api/claw?op=debug_agent_eval&id=ag_xxx[&run=1]
    const id = url.searchParams.get("id") ?? "";
    if (!id) return jsonOk({ error: "id is required" });
    const { getSubAgent } = await import("@/app/lib/agents");
    const { listAgentEvalScores } = await import("@/app/lib/agentEvals");
    const agent = await getSubAgent(id);
    if (!agent) return jsonOk({ error: "agent not found" });

    const recent = await listAgentEvalScores(id, 5);

    let verbose: unknown = null;
    if (url.searchParams.get("run") === "1") {
      const { generateText } = await import("ai");
      const { buildLlmArgs } = await import("@/app/lib/modelRouting");
      const core = (purpose: "meta" | "fast-meta") => {
        const llm = buildLlmArgs({ purpose }) as any;
        return {
          model: llm.model,
          ...(llm.providerOptions ? { providerOptions: llm.providerOptions } : {}),
          ...(typeof llm.temperature === "number" ? { temperature: llm.temperature } : {}),
        };
      };
      const taskGen = await generateText({
        ...core("fast-meta"),
        system:
          "Given an autonomous specialist agent's mission, write ONE concrete, " +
          "realistic task it would actually face. Output ONLY the task, in one " +
          "or two sentences — no preamble.",
        prompt: [
          `AGENT: ${agent.emoji} ${agent.name}`,
          `MISSION:\n${agent.persona}`,
          `TOOLKITS: [${agent.toolkits.join(", ")}]`,
        ].join("\n\n"),
      });
      const probeTask = (taskGen.text ?? "").trim().slice(0, 500);
      const outGen = await generateText({
        ...core("meta"),
        system:
          `You are ${agent.name}. ${agent.persona}\n\n` +
          "Produce the actual deliverable the task asks for (draft form is fine — do not claim to " +
          "have sent or executed anything). Be concrete and complete.",
        prompt: probeTask,
      });
      verbose = { probeTask, output: (outGen.text ?? "").slice(0, 4000) };
    }

    return jsonOk({
      agent: {
        id: agent.id,
        name: agent.name,
        emoji: agent.emoji,
        persona: agent.persona,
        toolkits: agent.toolkits,
        skills: agent.skills ?? null,
        telegramBotId: agent.telegramBotId ?? null,
      },
      recentScores: recent.map((s) => ({
        overall: s.overall,
        ts: s.ts,
        note: s.note ?? null,
        dimensions: s.dimensions,
      })),
      verbose,
    });
  }

  if (op === "eval_workforces") {
    // Run an eval pass across a tenant's workforces NOW, so the agent-evals
    // graph populates without waiting for organic live runs. Resolves the
    // tenant like the dashboard does (?tenant= override, else owner). Walks
    // every workforce's member + candidate agents (deduped), and for each one
    // synthesizes a representative task, produces a side-effect-free
    // deliverable through its persona, and grades it.
    //   /api/claw?op=eval_workforces[&tenant=telegram:123][&limit=12]
    const { resolveUiTenant } = await import("@/app/lib/uiTenant");
    const tenant =
      url.searchParams.get("tenant") || (await resolveUiTenant(null));
    if (!tenant) return jsonOk({ error: "could not resolve tenant" });

    const { listWorkforcesByTenant, listAgentsByTenant } = await import(
      "@/app/lib/agents"
    );
    const { evalAgentStep } = await import("@/app/steps/agentEvalSteps");

    const [teams, allAgents] = await Promise.all([
      listWorkforcesByTenant(tenant),
      listAgentsByTenant(tenant),
    ]);
    const agentIds = new Set(allAgents.map((a) => a.id));

    const memberIds = new Set<string>();
    for (const t of teams) {
      for (const st of t.stages) {
        if (st.kind === "agents") {
          for (const id of st.agentIds) if (agentIds.has(id)) memberIds.add(id);
        } else if (st.kind === "route") {
          for (const id of st.candidateAgentIds)
            if (agentIds.has(id)) memberIds.add(id);
        }
      }
    }

    // Fall back to every tenant agent when no workforce members resolve, so the
    // view still loads existing agents.
    let targetIds = [...memberIds];
    if (targetIds.length === 0) targetIds = allAgents.map((a) => a.id);

    const limit = Math.max(
      1,
      Math.min(20, Number(url.searchParams.get("limit") ?? "12"))
    );
    targetIds = targetIds.slice(0, limit);

    const runId = `eval_manual_${Date.now()}`;
    const results: Array<Awaited<ReturnType<typeof evalAgentStep>>> = [];
    for (const id of targetIds) {
      results.push(await evalAgentStep({ agentId: id, runId }));
    }

    return jsonOk({
      tenant,
      workforces: teams.length,
      evaluated: results.length,
      runId,
      results,
    });
  }

  if (op === "debug_auto") {
    // Inspect / pause / resume an automation, and view its recent run backlog
    // — used to stop a runaway automation and see whether runs are a draining
    // backlog or actively re-firing. action=get|pause|resume|runs.
    const id = url.searchParams.get("id") ?? "";
    const action = url.searchParams.get("action") ?? "get";
    if (!id) return jsonOk({ error: "id is required" });
    const rule = await getAutomation(id);
    if (!rule) return jsonOk({ error: "automation not found" });
    if (action === "pause") {
      const ok = await setAutomationEnabled(id, false);
      return jsonOk({ ok, id, enabled: false, status: "paused" });
    }
    if (action === "resume") {
      const ok = await setAutomationEnabled(id, true);
      return jsonOk({ ok, id, enabled: true, status: "active" });
    }
    if (action === "match") {
      // Does matchComposio find this rule for its own trigger type? Surfaces
      // index drift (rule missing from auto:by_trigger:<type>).
      const { matchComposio } = await import("@/app/lib/automations");
      const tt =
        rule.trigger.kind === "composio" ? rule.trigger.triggerType : "(not-composio)";
      const matches = await matchComposio(tt, rule.tenantId);
      return jsonOk({
        id,
        triggerType: tt,
        tenantId: rule.tenantId,
        matchedIds: matches.map((m) => m.id),
        selfMatched: matches.some((m) => m.id === id),
      });
    }
    if (action === "fire") {
      // Manually fire the automation with a synthetic event to exercise the
      // full run path (prepare → single agent turn → finalize).
      const { fireAutomation } = await import("@/app/lib/automations");
      const event = { manual_test: true, ts: Date.now(), message_id: `manual_${Date.now()}` };
      const runId = await fireAutomation(id, "chat", event);
      return jsonOk({ id, fired: Boolean(runId), runId });
    }
    if (action === "event") {
      // Dump the full stored run record (incl. event payload + source) so we
      // can see why dedupe let two near-simultaneous deliveries through.
      const { getRun } = await import("@/app/lib/automations");
      const runId = url.searchParams.get("runId") ?? "";
      const run = await getRun(runId);
      return jsonOk({ id, runId, run });
    }
    if (action === "runs") {
      const { listRunsByRule } = await import("@/app/lib/automations");
      const limit = Math.max(1, Math.min(100, Number(url.searchParams.get("limit") ?? "30")));
      const runs = await listRunsByRule(id, limit);
      return jsonOk({
        id,
        enabled: rule.enabled,
        fireCount: rule.fireCount,
        count: runs.length,
        runs: runs.map((r) => ({
          id: r.id,
          ts: r.ts,
          status: r.status,
          jobId: r.jobId,
          error: r.error,
        })),
      });
    }
    return jsonOk({
      id,
      name: rule.name,
      enabled: rule.enabled,
      status: rule.status,
      trigger: rule.trigger,
      action: rule.action,
      fireCount: rule.fireCount,
      lastFiredAt: rule.lastFiredAt,
      lastRunId: rule.lastRunId,
    });
  }

  if (op === "cron") {
    return handleCronTrigger();
  }

  if (op === "whatsapp") {
    const v = whatsappVerifyChallenge(url);

    if (v.ok) return new Response(v.challenge ?? "", { status: 200 });

    return new Response("Verification failed", { status: 403 });
  }

  if (op === "media") {
    const raw = url.searchParams.get("url") ?? "";
    if (!raw) return new Response("Missing url param", { status: 400 });

    const decoded = safeDecodeMediaUrlParam(decodeURIComponent(raw));

    let u: URL;

    try {
      u = new URL(decoded);
    } catch {
      return new Response("Bad url", { status: 400 });
    }

    if (!MEDIA_ALLOWED_HOSTS.has(u.host)) {
      return new Response("Host not allowed", { status: 403 });
    }

    const res = await fetch(u.toString(), {
      method: "GET",
    });

    if (!res.ok) {
      return new Response(`Upstream error: ${res.status}`, {
        status: 502,
      });
    }

    const contentType = res.headers.get("content-type") ?? "application/octet-stream";

    const headers = new Headers();
    headers.set("content-type", contentType);
    headers.set("cache-control", "public, max-age=31536000, immutable");

    const etag = res.headers.get("etag");
    if (etag) headers.set("etag", etag);

    const lastMod = res.headers.get("last-modified");
    if (lastMod) headers.set("last-modified", lastMod);

    return new Response(res.body, {
      status: 200,
      headers,
    });
  }

  if (op === "webhook") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const body = await req.json().catch(() => null);
    if (!body) return new Response("Bad JSON", { status: 400 });

    const message = String(body.message ?? "");
    if (!message) return new Response("Missing field: message", { status: 400 });

    const deliver = body.deliver !== undefined ? Boolean(body.deliver) : true;
    const channel = String(body.channel ?? "last");
    const allowSessionOverride = env("ALLOW_WEBHOOK_SESSION_ID") === "true";
    const requestedSessionId = allowSessionOverride ? String(body.sessionId ?? "") : "";

    let target: { channel: Channel; sessionId: string } | null = null;

    if (requestedSessionId) {
      const meta = await getSessionMeta(requestedSessionId);
      if (meta) target = { channel: meta.channel, sessionId: meta.sessionId };
    } else if (channel === "last") {
      target = await getLastSession("any");
    } else if (channel === "telegram" || channel === "whatsapp" || channel === "sms") {
      target = await getLastSession(channel);
    }

    if (!deliver) return new Response(null, { status: 202 });
    if (!target) return new Response("No active chat session to deliver to", { status: 409 });

    const meta = await getSessionMeta(target.sessionId);
    if (!meta) return new Response("Missing session metadata", { status: 409 });

    const synthetic: InboundMessage = {
      channel: meta.channel,
      sessionId: meta.sessionId,
      senderId: meta.senderId,
      senderUsername: meta.senderUsername,
      text: message,
      ts: Date.now(),
      raw: {
        source: "webhook",
      },
    };

    await routeToSession(synthetic);

    return new Response(null, { status: 202 });
  }

  if (op === "telegram") {
    return handleTelegramWebhook(req);
  }

  if (op === "status") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const jobId = url.searchParams.get("jobId") ?? "";
    if (!jobId) {
      const tenantId = url.searchParams.get("tenantId") ?? "";
      if (!tenantId) {
        return new Response("Missing jobId or tenantId", { status: 400 });
      }
      const [active, recent] = await Promise.all([
        listActiveJobs(tenantId),
        listRecentJobs(tenantId, 20),
      ]);
      return NextResponse.json({ ok: true, active, recent });
    }

    const meta = await getJobMeta(jobId);
    if (!meta) return new Response("Job not found", { status: 404 });

    const limit = Math.max(
      1,
      Math.min(200, Number(url.searchParams.get("thoughts") ?? "20"))
    );
    const thoughts = await getThoughts(jobId, { limit });
    return NextResponse.json({ ok: true, meta, thoughts });
  }

  if (op === "ask") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const jobId = url.searchParams.get("jobId") ?? "";
    const question = url.searchParams.get("q") ?? "";
    if (!jobId || !question) {
      return new Response("Missing jobId or q", { status: 400 });
    }

    try {
      const result = await askJob({ jobId, question });
      if (!result.ok) return new Response("Job not found", { status: 404 });
      return NextResponse.json(result);
    } catch (err: any) {
      return new Response(
        `Ask failed: ${err?.message ?? String(err)}`,
        { status: 500 }
      );
    }
  }

  // Live workforce diagrams as a WDK manifest fragment. Ungated: the dashboard's
  // patched fetchWorkflowsManifest calls this (GET, no bearer) to merge
  // user-created teams into the manifest object at request time.
  if (op === "workforce_manifest") {
    try {
      const { buildWorkforceManifestFragment } = await import(
        "@/app/lib/workforceManifest"
      );
      const frag = await buildWorkforceManifestFragment();
      return NextResponse.json(frag, {
        headers: { "cache-control": "no-store" },
      });
    } catch (e: any) {
      return NextResponse.json(
        { workflows: {}, error: String(e?.message ?? e).slice(0, 300) },
        { status: 200 }
      );
    }
  }

  return new Response("Not found", { status: 404 });
}

// ============================================================
// POST handler
// ============================================================
export async function POST(req: Request) {
  const url = new URL(req.url);
  const op = url.searchParams.get("op");

  if (op === "cron") {
    return handleCronTrigger();
  }

  // Per-automation inbound webhook (external systems POST here to fire a rule).
  if (op === "auto_webhook") {
    const id = url.searchParams.get("id") ?? "";
    const secret = url.searchParams.get("secret") ?? "";
    if (!id || !secret) return new Response("Missing id/secret", { status: 400 });
    const rule = await getAutomation(id);
    if (!rule || rule.trigger.kind !== "webhook" || rule.trigger.secret !== secret) {
      return new Response("Unauthorized", { status: 401 });
    }
    if (!rule.enabled) return new Response("Automation paused", { status: 409 });
    const body = await req.json().catch(() => ({}));
    const runId = await fireAutomation(id, "webhook", body);
    return NextResponse.json({ ok: true, runId }, { status: 202 });
  }

  if (op === "pair") {
    await ensurePairingCode();

    const code = req.headers.get("x-pairing-code") ?? "";
    if (!code) return new Response("Missing X-Pairing-Code header", { status: 401 });

    const token = await exchangePairingCode(code);
    if (!token) return new Response("Invalid pairing code", { status: 401 });

    return jsonOk({ token });
  }

  if (op === "webhook") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const body = await req.json().catch(() => null);
    if (!body) return new Response("Bad JSON", { status: 400 });

    const message = String(body.message ?? "");
    if (!message) return new Response("Missing field: message", { status: 400 });

    const deliver = body.deliver !== undefined ? Boolean(body.deliver) : true;
    const channel = String(body.channel ?? "last");
    const allowSessionOverride = env("ALLOW_WEBHOOK_SESSION_ID") === "true";
    const requestedSessionId = allowSessionOverride ? String(body.sessionId ?? "") : "";

    let target: { channel: Channel; sessionId: string } | null = null;

    if (requestedSessionId) {
      const meta = await getSessionMeta(requestedSessionId);
      if (meta) target = { channel: meta.channel, sessionId: meta.sessionId };
    } else if (channel === "last") {
      target = await getLastSession("any");
    } else if (channel === "telegram" || channel === "whatsapp" || channel === "sms") {
      target = await getLastSession(channel);
    }

    if (!deliver) return new Response(null, { status: 202 });
    if (!target) return new Response("No active chat session to deliver to", { status: 409 });

    const meta = await getSessionMeta(target.sessionId);
    if (!meta) return new Response("Missing session metadata", { status: 409 });

    const synthetic: InboundMessage = {
      channel: meta.channel,
      sessionId: meta.sessionId,
      senderId: meta.senderId,
      senderUsername: meta.senderUsername,
      text: message,
      ts: Date.now(),
      raw: {
        source: "webhook",
      },
    };

    await routeToSession(synthetic);

    return new Response(null, { status: 202 });
  }

  if (op === "telegram") {
    return handleTelegramWebhook(req);
  }

  // Dedicated sub-agent bot webhook: /api/claw?op=agent_telegram&bot=<botId>.
  // Validated against the per-bot secret minted at /agent bind time. The
  // sender chats with the agent's own bot; tenant identity is FORCED to the
  // bot owner's tenant so VFS / Composio / memory scopes line up, and the
  // session id (`tgagent:…`) makes every outbound reply use the bot's token.
  if (op === "agent_telegram") {
    const botId = url.searchParams.get("bot") ?? "";
    const bot = await getAgentBot(botId);
    if (!bot) return new Response("Unknown bot", { status: 404 });
    const got = req.headers.get("x-telegram-bot-api-secret-token");
    if (got !== bot.secret) return new Response("Unauthorized", { status: 401 });

    const update = (await req.json().catch(() => null)) as any;
    if (!update) return new Response("Bad JSON", { status: 400 });

    const updateId = update?.update_id;
    if (typeof updateId === "number") {
      const inserted = await getStore().set(
        `dedupe:tgagent:${botId}:update:${updateId}`,
        "1",
        { exSeconds: 600, nx: true }
      );
      if (!inserted) return jsonOk({ deduped: true });
    }

    const message = update?.message;
    const text =
      typeof message?.text === "string"
        ? message.text
        : typeof message?.caption === "string"
          ? message.caption
          : "";
    const chatId = message?.chat?.id;
    if (!chatId || !text.trim()) return jsonOk({ ignored: true });

    const agent = await getSubAgent(bot.agentId);
    if (!agent) return jsonOk({ error: "agent record missing for this bot" });

    const threadId = message?.message_thread_id;
    const sessionId = `tgagent:${botId}:${chatId}${threadId ? `:${threadId}` : ""}`;

    await start(agentChatWorkflow, [
      {
        sessionId,
        tenantId: bot.tenantId,
        text: text.trim(),
        agent: scopeForAgent(agent),
      },
    ]);
    return jsonOk({ queued: true, agent: agent.name });
  }

  if (op === "sms") {
    const raw = await req.text();

    const apiKey = getTextbeltApiKeyOptional();

    if (apiKey && shouldVerifyTextbeltWebhook()) {
      const sig = req.headers.get("x-textbelt-signature");
      const ts = req.headers.get("x-textbelt-timestamp");

      const ok = await verifyTextbeltWebhook({
        apiKey,
        timestampHeader: ts,
        signatureHeader: sig,
        rawBody: raw,
      });

      if (!ok) return new Response("Invalid Textbelt signature", { status: 401 });
    }

    const body = JSON.parse(raw);
    const msg = normalizeTextbeltReply(body);

    if (msg) await handleInbound(msg);

    return jsonOk();
  }

  if (op === "whatsapp") {
    const raw = await req.text();
    const sig = req.headers.get("x-hub-signature-256");

    if (!(await verifyWhatsAppSignature(raw, sig))) {
      return new Response("Invalid signature", { status: 401 });
    }

    const body = JSON.parse(raw);
    const messages = normalizeWhatsApp(body);

    for (const m of messages) {
      await handleInbound(m);
    }

    return jsonOk();
  }

  // Composio fires events here when a user's subscribed trigger matches.
  // The endpoint is intentionally NOT behind verifyGatewayBearer — Composio
  // doesn't know about our bearer flow. We verify their HMAC signature
  // instead when COMPOSIO_WEBHOOK_SECRET is set.
  if (op === "composio_webhook") {
    const raw = await req.text();
    console.log(
      `[composio_webhook] received ${raw.length}b body; headers webhook-id=${req.headers.get("webhook-id") ?? "(none)"}`
    );
    // Pass the raw body + Svix-style headers to the dispatcher, which handles
    // signature verification (when COMPOSIO_WEBHOOK_SECRET is set) AND
    // version-detecting the V1/V2/V3 payload.
    const result = await dispatchComposioWebhook({
      rawBody: raw,
      headers: {
        "webhook-id": req.headers.get("webhook-id"),
        "webhook-timestamp": req.headers.get("webhook-timestamp"),
        "webhook-signature": req.headers.get("webhook-signature"),
      },
    });
    if (!result.ok) {
      console.warn(`[composio_webhook] not delivered: ${result.error}`);
      // 200 so Composio doesn't hammer retries for an undeliverable event,
      // but include the error so it's visible in their delivery logs.
      return NextResponse.json({ ok: false, error: result.error });
    }
    return NextResponse.json({ ok: true });
  }

  if (op === "dispatch") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const body = await req.json().catch(() => null);
    if (!body) return new Response("Bad JSON", { status: 400 });

    const prompt = String(body.prompt ?? body.message ?? "").trim();
    if (!prompt) return new Response("Missing field: prompt", { status: 400 });

    const channelArg = String(body.channel ?? "last");
    const allowSessionOverride = env("ALLOW_WEBHOOK_SESSION_ID") === "true";
    const requestedSessionId = allowSessionOverride
      ? String(body.sessionId ?? "")
      : "";

    let target: { channel: Channel; sessionId: string } | null = null;
    let resolvedSenderId = String(body.senderId ?? "");

    if (requestedSessionId) {
      const meta = await getSessionMeta(requestedSessionId);
      if (meta) {
        target = { channel: meta.channel, sessionId: meta.sessionId };
        resolvedSenderId = resolvedSenderId || meta.senderId;
      }
    } else if (channelArg === "last") {
      target = await getLastSession("any");
      if (target) {
        const meta = await getSessionMeta(target.sessionId);
        if (meta) resolvedSenderId = resolvedSenderId || meta.senderId;
      }
    } else if (
      channelArg === "telegram" ||
      channelArg === "whatsapp" ||
      channelArg === "sms"
    ) {
      target = await getLastSession(channelArg);
      if (target) {
        const meta = await getSessionMeta(target.sessionId);
        if (meta) resolvedSenderId = resolvedSenderId || meta.senderId;
      }
    }

    if (!target) {
      return new Response("No active chat session to dispatch into", {
        status: 409,
      });
    }
    if (!resolvedSenderId) {
      return new Response("Could not resolve senderId for tenant scoping", {
        status: 409,
      });
    }

    const out = await dispatchJob({
      channel: target.channel,
      sessionId: target.sessionId,
      senderId: resolvedSenderId,
      prompt,
    });

    return NextResponse.json({
      ok: true,
      jobId: out.jobId,
      deep: out.deep,
      reason: out.reason,
    });
  }

  if (op === "set_last_session") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const body = await req.json().catch(() => null);
    if (!body) return new Response("Bad JSON", { status: 400 });

    // userId looks like "<channel>:<id>", e.g. "telegram:1236381479".
    const raw = String(body.userId ?? body.sessionId ?? "").trim();
    const m = /^([a-z]+):(.+)$/i.exec(raw);
    if (!m) {
      return new Response(
        `Invalid userId "${raw}". Expected "<channel>:<id>", e.g. telegram:1236381479.`,
        { status: 400 }
      );
    }
    const channel = m[1].toLowerCase() as Channel;
    const id = m[2];
    const sessionId = `${channel}:${id}`;

    await saveSessionMeta(
      {
        channel,
        sessionId,
        senderId: id,
        updatedAt: Date.now(),
      },
      { updateLast: true }
    );

    return NextResponse.json({ ok: true, last: { channel, sessionId } });
  }

  if (op === "discover_triggers") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const body = await req.json().catch(() => null);
    const toolkit = body?.toolkit ? String(body.toolkit) : undefined;
    const keyword = body?.keyword ? String(body.keyword) : undefined;
    const limit = Number(body?.limit ?? 50);

    const triggers = await listTriggerTypes({
      toolkits: toolkit ? [toolkit] : undefined,
      keyword,
      limit: Number.isFinite(limit) ? limit : 50,
    });
    return NextResponse.json({
      ok: true,
      count: triggers.length,
      triggers: triggers.map((t) => ({
        slug: t.slug,
        name: t.name,
        description: t.description,
        toolkit: t.toolkitSlug,
        config_schema: t.configSchema,
      })),
    });
  }

  // Live workforce diagrams as a WDK manifest fragment. Ungated: the dashboard's
  // patched fetchWorkflowsManifest calls this server-to-server (no bearer) to
  // merge user-created teams into the manifest object at request time. Only
  // exposes presentation graphs, no secrets.
  if (op === "workforce_manifest") {
    try {
      const { buildWorkforceManifestFragment } = await import(
        "@/app/lib/workforceManifest"
      );
      const frag = await buildWorkforceManifestFragment();
      return NextResponse.json(frag, {
        headers: { "cache-control": "no-store" },
      });
    } catch (e: any) {
      return NextResponse.json(
        { workflows: {}, error: String(e?.message ?? e).slice(0, 300) },
        { status: 200 }
      );
    }
  }

  if (op === "debug_manifest") {
    const ok = await verifyGatewayBearer(req);
    if (!ok) return new Response("Unauthorized", { status: 401 });

    const fs = await import("node:fs/promises");
    const path = await import("node:path");
    const cwd = process.cwd();
    const candidates = [
      process.env.WORKFLOW_MANIFEST_PATH ?? null,
      path.join(cwd, "app/.well-known/workflow/v1/manifest.json"),
      path.join(cwd, "public/.well-known/workflow/v1/manifest.json"),
      "/var/task/app/.well-known/workflow/v1/manifest.json",
      "/var/task/public/.well-known/workflow/v1/manifest.json",
    ].filter((p): p is string => !!p);

    const results = [];
    for (const p of candidates) {
      try {
        const txt = await fs.readFile(p, "utf8");
        const m = JSON.parse(txt);
        const wfFiles = m.workflows ?? {};
        const names = Object.values<any>(wfFiles).flatMap((g) =>
          Object.keys(g ?? {})
        );
        const code =
          wfFiles["app/workflows/codeWorkflow.ts"]?.codeWorkflow?.graph?.nodes
            ?.length ?? null;
        const nbriFile = wfFiles["app/workflows/nbri.ts"];
        const nbri = nbriFile ? Object.keys(nbriFile) : null;
        const nbriDetail = nbriFile
          ? Object.fromEntries(
              Object.entries<any>(nbriFile).map(([name, wf]) => {
                const g = wf?.graph ?? {};
                const kinds: Record<string, number> = {};
                for (const n of g.nodes ?? []) {
                  const k = n?.data?.nodeKind ?? "?";
                  kinds[k] = (kinds[k] ?? 0) + 1;
                }
                const hookEdges = (g.edges ?? []).filter(
                  (e: any) => e?.label === "hook"
                ).length;
                return [name, { nodeKinds: kinds, hookEdges }];
              })
            )
          : null;
        results.push({
          path: p,
          exists: true,
          size: txt.length,
          workflowFileCount: Object.keys(wfFiles).length,
          names,
          codeWorkflowNodes: code,
          nbri,
          nbriDetail,
        });
      } catch (e: any) {
        results.push({ path: p, exists: false, error: String(e?.code ?? e?.message ?? e).slice(0, 160) });
      }
    }
    return NextResponse.json({ ok: true, cwd, candidates: results });
  }

  return new Response("Not found", { status: 404 });
}
