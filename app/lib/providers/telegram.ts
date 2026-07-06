// app/lib/providers/telegram.ts
import { env, envRequired } from "@/app/lib/env";

type TelegramApiOk<T> = { ok: true; result: T };
type TelegramApiErr = { ok: false; description?: string; error_code?: number };

type TelegramChatAction =
  | "typing"
  | "upload_photo"
  | "record_video"
  | "upload_video"
  | "record_voice"
  | "upload_voice"
  | "upload_document"
  | "choose_sticker"
  | "find_location"
  | "record_video_note"
  | "upload_video_note";

type TelegramParseMode = "HTML";
type TelegramMediaInput = string | Blob | File;

type TelegramPreparedText = {
  html: string;
  plain: string;
  parseMode: TelegramParseMode;
};

const TELEGRAM_PARSE_MODE: TelegramParseMode = "HTML";
const TELEGRAM_SUPPORTED_HTML_ENTITY_RE = /&(lt|gt|amp|quot|#\d+|#x[0-9a-f]+);/gi;
const TELEGRAM_CODE_FENCE_RE = /```([^\n`]*)\n?([\s\S]*?)```/g;
const TELEGRAM_INLINE_CODE_RE = /`([^`\n]+)`/g;
const TELEGRAM_LOG_LINE_RE =
  /(^\s*(?:\d{4}-\d{2}-\d{2}[T\s]\d{2}:\d{2}:\d{2}|\d{2}:\d{2}:\d{2}|\[(?:TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\]|(?:TRACE|DEBUG|INFO|WARN|WARNING|ERROR|FATAL)\b|at\s+\S+\s*\(|Caused by:|Exception:|Traceback \(most recent call last\):))/i;
const TELEGRAM_ENTITY_ERROR_RE = /can't parse entities|unsupported start tag|unexpected end tag|entity name expected|tag ".*?" must be closed|bad request: can't parse/i;

const TELEGRAM_LANGUAGE_ALIASES: Record<string, string> = {
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  py: "python",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  fish: "bash",
  console: "bash",
  ps1: "powershell",
  psql: "sql",
  yml: "yaml",
  md: "markdown",
  plaintext: "text",
  plain: "text",
  txt: "text",
  text: "text",
  logs: "log",
  log: "log",
  conf: "ini",
  cfg: "ini",
  dockerfile: "dockerfile",
  html: "html",
  xml: "xml",
  svg: "xml",
  json: "json",
  yaml: "yaml",
  sql: "sql",
  diff: "diff",
  ini: "ini",
  toml: "toml",
  rust: "rust",
  rs: "rust",
  go: "go",
  java: "java",
  kt: "kotlin",
  kotlin: "kotlin",
  swift: "swift",
  c: "c",
  h: "c",
  cpp: "cpp",
  cxx: "cpp",
  cc: "cpp",
  hpp: "cpp",
  cs: "csharp",
  php: "php",
  rb: "ruby",
  ruby: "ruby",
};

export function telegramSessionToChatAndThread(sessionId: string): { chatId: string; threadId?: number } {
  // sessionId shapes:
  //   telegram:<chatId>                        main bot
  //   telegram:<chatId>:<threadId>             main bot, forum thread
  //   tgagent:<botId>:<chatId>                 dedicated sub-agent bot
  //   tgagent:<botId>:<chatId>:<threadId>      dedicated sub-agent bot, thread
  const parts = String(sessionId ?? "").split(":");
  if (parts[0] === "tgagent") {
    const chatId = parts[2] ?? "";
    const threadId = parts.length >= 4 ? Number(parts[3]) : undefined;
    return { chatId, threadId: Number.isFinite(threadId as any) ? threadId : undefined };
  }
  const chatId = parts[1] ?? "";
  const threadId = parts.length >= 3 ? Number(parts[2]) : undefined;
  return { chatId, threadId: Number.isFinite(threadId as any) ? threadId : undefined };
}

// Per-bot token resolution. `tgagent:<botId>:…` sessions send via that bot's
// BotFather token (loaded from Redis, cached 60s); everything else uses the
// main TELEGRAM_BOT_TOKEN. Resolving INSIDE this module means every send /
// edit / typing / media call site works for agent bots with no changes.
const agentBotTokenCache = new Map<string, { token: string; ts: number }>();
const AGENT_BOT_TOKEN_TTL_MS = 60_000;

export async function resolveTelegramTokenForSession(sessionId: string): Promise<string> {
  const parts = String(sessionId ?? "").split(":");
  if (parts[0] !== "tgagent") return envRequired("TELEGRAM_BOT_TOKEN");
  const botId = parts[1] ?? "";
  const cached = agentBotTokenCache.get(botId);
  if (cached && Date.now() - cached.ts < AGENT_BOT_TOKEN_TTL_MS) return cached.token;
  const { getAgentBot } = await import("@/app/lib/agents");
  const bot = await getAgentBot(botId);
  if (!bot) throw new Error(`No telegram bot record for ${botId}`);
  agentBotTokenCache.set(botId, { token: bot.token, ts: Date.now() });
  return bot.token;
}

export async function telegramValidateWebhook(req: Request): Promise<boolean> {
  const secret = env("TELEGRAM_WEBHOOK_SECRET");
  if (!secret) return true;
  const got = req.headers.get("x-telegram-bot-api-secret-token");
  return got === secret;
}

function normalizeTelegramText(text: string): string {
  return String(text ?? "").replace(/\r\n?/g, "\n");
}

function escapeTelegramHtml(text: string): string {
  return normalizeTelegramText(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function stripSupportedEntities(text: string): string {
  return text.replace(TELEGRAM_SUPPORTED_HTML_ENTITY_RE, "X");
}

function normalizeTelegramLanguage(language?: string | null, code?: string): string | undefined {
  const raw = String(language ?? "")
    .trim()
    .toLowerCase()
    .replace(/^language-/, "")
    .replace(/[^a-z0-9#+._-]/g, "");

  if (raw) {
    return TELEGRAM_LANGUAGE_ALIASES[raw] ?? raw;
  }

  return detectTelegramCodeLanguage(code ?? "");
}

function looksLikeJsonBlock(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed || !/^[\[{]/.test(trimmed)) return false;

  try {
    JSON.parse(trimmed);
    return true;
  } catch {
    return false;
  }
}

function looksLikeLogBlock(text: string): boolean {
  const lines = normalizeTelegramText(text)
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);

  if (lines.length < 2) return false;

  const matchingLines = lines.filter((line) => TELEGRAM_LOG_LINE_RE.test(line)).length;
  return matchingLines >= Math.max(2, Math.ceil(lines.length * 0.5));
}

function looksLikeShellBlock(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;

  if (/^#!\/.*\b(?:bash|sh|zsh|fish)\b/m.test(trimmed)) return true;

  const lines = trimmed.split("\n").map((line) => line.trim());
  const commandish = lines.filter(
    (line) =>
      /^\$\s+/.test(line) ||
      /^(?:npm|pnpm|yarn|bun|node|npx|git|curl|wget|ls|cd|cat|echo|grep|find|cp|mv|rm|mkdir|chmod|chown|docker|kubectl|ssh|scp|rsync)\b/.test(line)
  ).length;

  return commandish >= Math.max(1, Math.ceil(lines.length * 0.5));
}

function looksLikeSqlBlock(text: string): boolean {
  const trimmed = text.trim();
  return /^(?:select|insert|update|delete|create|alter|drop|with)\b/i.test(trimmed);
}

function looksLikeHtmlOrXmlBlock(text: string): boolean {
  const trimmed = text.trim();
  return /^<(?:!doctype\s+html|html|body|head|div|span|svg|\?xml|[a-z][a-z0-9:_-]*)(?:\s|>|\/)/i.test(trimmed);
}

function looksLikePythonBlock(text: string): boolean {
  return /(^|\n)\s*(?:def\s+\w+\s*\(|class\s+\w+[(:]|from\s+\S+\s+import\s+|import\s+\S+|print\s*\()/m.test(text);
}

function looksLikeTypeScriptBlock(text: string): boolean {
  return /(^|\n)\s*(?:interface\s+\w+|type\s+\w+\s*=|export\s+type\s+|export\s+interface\s+|const\s+\w+\s*:\s*\w|function\s+\w+\s*<)/m.test(text);
}

function looksLikeJavaScriptBlock(text: string): boolean {
  return /(^|\n)\s*(?:const\s+\w+\s*=|let\s+\w+\s*=|var\s+\w+\s*=|function\s+\w+\s*\(|import\s+.+\s+from\s+['"]|export\s+(?:default\s+)?(?:function|const|class)|class\s+\w+|\w+\s*=>)/m.test(text);
}

function detectTelegramCodeLanguage(code: string): string | undefined {
  const text = normalizeTelegramText(code);
  const trimmed = text.trim();
  if (!trimmed) return undefined;

  if (looksLikeJsonBlock(trimmed)) return "json";
  if (looksLikeLogBlock(trimmed)) return "log";
  if (looksLikeSqlBlock(trimmed)) return "sql";
  if (looksLikeHtmlOrXmlBlock(trimmed)) return /^<\?xml|^<svg\b/i.test(trimmed) ? "xml" : "html";
  if (looksLikeShellBlock(trimmed)) return "bash";
  if (looksLikePythonBlock(trimmed)) return "python";
  if (looksLikeTypeScriptBlock(trimmed)) return "typescript";
  if (looksLikeJavaScriptBlock(trimmed)) return "javascript";
  if (/^\s*---\s*$/m.test(trimmed) || /^\s*[A-Za-z0-9_.-]+\s*:\s*.+$/m.test(trimmed)) return "yaml";

  return undefined;
}

function shouldRenderEntireMessageAsCodeBlock(text: string): boolean {
  // Disabled: previously this auto-wrapped any message that "looked" like
  // JSON / logs / yaml / code into a monospace block. In a casual chat that
  // made ordinary replies render as gray code blocks, which felt robotic.
  // The model still gets monospace when it explicitly fences code with ```.
  void text;
  return false;
}

function renderInlineCodeHtml(text: string): string {
  const normalized = normalizeTelegramText(text);
  let out = "";
  let lastIndex = 0;

  for (const match of normalized.matchAll(TELEGRAM_INLINE_CODE_RE)) {
    const index = match.index ?? 0;
    out += escapeTelegramHtml(normalized.slice(lastIndex, index));
    out += `<code>${escapeTelegramHtml(match[1] ?? "")}</code>`;
    lastIndex = index + match[0].length;
  }

  out += escapeTelegramHtml(normalized.slice(lastIndex));
  return out;
}

function renderCodeBlockHtml(code: string, language?: string | null): string {
  // Intentionally NO `class="language-X"` — that attribute is what makes
  // Telegram colorize the block with syntax highlighting, which the user
  // finds noisy. Plain <pre><code> keeps the monospace block (still useful
  // for real code) without the rainbow.
  void language;
  const normalizedCode = normalizeTelegramText(code).replace(/^\n/, "").replace(/\n$/, "");
  const escaped = escapeTelegramHtml(normalizedCode);
  return `<pre><code>${escaped}</code></pre>`;
}

function convertMarkdownFencesToTelegramHtml(text: string): string {
  const normalized = normalizeTelegramText(text);

  if (!normalized.includes("```")) {
    if (shouldRenderEntireMessageAsCodeBlock(normalized)) {
      return renderCodeBlockHtml(normalized, undefined);
    }
    return renderInlineCodeHtml(normalized);
  }

  const fenceCount = normalized.match(/```/g)?.length ?? 0;
  if (fenceCount % 2 !== 0) {
    return renderInlineCodeHtml(normalized);
  }

  let out = "";
  let lastIndex = 0;
  let matchedFence = false;

  for (const match of normalized.matchAll(TELEGRAM_CODE_FENCE_RE)) {
    matchedFence = true;
    const index = match.index ?? 0;
    out += renderInlineCodeHtml(normalized.slice(lastIndex, index));
    out += renderCodeBlockHtml(match[2] ?? "", match[1] ?? "");
    lastIndex = index + match[0].length;
  }

  if (!matchedFence) {
    return renderInlineCodeHtml(normalized);
  }

  out += renderInlineCodeHtml(normalized.slice(lastIndex));
  return out;
}

export function telegramFormatMessageHtml(text: string): TelegramPreparedText {
  const plain = normalizeTelegramText(text ?? "") || "…";
  const html = convertMarkdownFencesToTelegramHtml(plain) || "…";
  return {
    html,
    plain,
    parseMode: TELEGRAM_PARSE_MODE,
  };
}

function shouldRetryWithoutHtml(error: unknown): boolean {
  const message = String((error as any)?.message ?? error ?? "");
  return TELEGRAM_ENTITY_ERROR_RE.test(message);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

const TELEGRAM_RETRY_FATAL_CODES = new Set([401, 403, 404]);
const TELEGRAM_RETRY_SERVER_CODES = new Set([500, 502, 503, 504]);
const TELEGRAM_RETRY_SERVER_DELAYS_MS = [500, 1000, 2000];
const TELEGRAM_MAX_RETRIES = 3;

/**
 * Execute `attempt` up to TELEGRAM_MAX_RETRIES+1 times, applying retry logic:
 *   - HTTP 429: respect Retry-After header (default 1 s), retry up to 3 times.
 *   - HTTP 500/502/503/504: exponential backoff (500 ms / 1 s / 2 s), up to 3 retries.
 *   - HTTP 401/403/404: throw immediately, no retry.
 *
 * `attempt` must return a { status, retryAfterHeader } descriptor so this helper
 * can decide without re-running the fetch, then call `extract` to get the final value.
 */
async function telegramWithRetry<T>(
  attempt: () => Promise<{ status: number; retryAfterHeader: string | null; extract: () => Promise<T> }>
): Promise<T> {
  let lastError: unknown;

  for (let retry = 0; retry <= TELEGRAM_MAX_RETRIES; retry++) {
    const probe = await attempt();

    if (TELEGRAM_RETRY_FATAL_CODES.has(probe.status)) {
      // Throw immediately — extract() will produce the right error message.
      return probe.extract();
    }

    if (probe.status === 429) {
      if (retry >= TELEGRAM_MAX_RETRIES) {
        lastError = new Error(`Telegram rate-limited (HTTP 429) after ${TELEGRAM_MAX_RETRIES} retries`);
        break;
      }
      const retryAfterSec = Number(probe.retryAfterHeader ?? "1");
      const delayMs = (Number.isFinite(retryAfterSec) && retryAfterSec > 0 ? retryAfterSec : 1) * 1000;
      await sleep(delayMs);
      continue;
    }

    if (TELEGRAM_RETRY_SERVER_CODES.has(probe.status)) {
      if (retry >= TELEGRAM_MAX_RETRIES) {
        lastError = new Error(`Telegram server error (HTTP ${probe.status}) after ${TELEGRAM_MAX_RETRIES} retries`);
        break;
      }
      const delayMs = TELEGRAM_RETRY_SERVER_DELAYS_MS[retry] ?? 2000;
      await sleep(delayMs);
      continue;
    }

    // Success path (2xx) or application-level errors (ok:false) — let extract() handle it.
    return probe.extract();
  }

  throw lastError ?? new Error("Telegram request failed after retries");
}

function appendTelegramFormValue(form: FormData, key: string, value: unknown): void {
  if (value === undefined || value === null) return;
  form.append(key, typeof value === "string" ? value : String(value));
}

function appendTelegramMediaInput(
  form: FormData,
  key: string,
  media: TelegramMediaInput,
  fallbackFilename: string
): void {
  if (typeof media === "string") {
    form.append(key, media);
    return;
  }

  const maybeName = (media as any)?.name;
  const filename = typeof maybeName === "string" && maybeName.trim() ? maybeName.trim() : fallbackFilename;
  form.append(key, media, filename);
}

async function telegramApiCallFormData<T>(method: string, form: FormData, token?: string): Promise<T> {
  const tok = token ?? envRequired("TELEGRAM_BOT_TOKEN");
  const url = `https://api.telegram.org/bot${tok}/${method}`;

  return telegramWithRetry(async () => {
    const res = await fetch(url, {
      method: "POST",
      body: form,
    });

    const retryAfterHeader = res.headers.get("Retry-After");
    const status = res.status;

    const extract = async (): Promise<T> => {
      const raw = await res.text();

      let parsed: TelegramApiOk<T> | TelegramApiErr | null = null;
      try {
        parsed = JSON.parse(raw);
      } catch {
        // ignore
      }

      if (parsed && (parsed as any).ok === false) {
        const err = parsed as TelegramApiErr;
        const code = err.error_code ?? res.status;
        const desc = err.description ?? raw;
        throw new Error(`Telegram ${method} failed: ${code} ${desc}`);
      }

      if (!res.ok) throw new Error(`Telegram ${method} HTTP ${res.status}: ${raw}`);
      if (!parsed || (parsed as any).ok !== true) throw new Error(`Telegram ${method} bad response: ${raw}`);

      return (parsed as TelegramApiOk<T>).result;
    };

    return { status, retryAfterHeader, extract };
  });
}

async function telegramApiCall<T>(method: string, payload: Record<string, unknown>, token?: string): Promise<T> {
  const tok = token ?? envRequired("TELEGRAM_BOT_TOKEN");
  const url = `https://api.telegram.org/bot${tok}/${method}`;

  return telegramWithRetry(async () => {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
    });

    const retryAfterHeader = res.headers.get("Retry-After");
    const status = res.status;

    const extract = async (): Promise<T> => {
      const raw = await res.text();

      let parsed: TelegramApiOk<T> | TelegramApiErr | null = null;
      try {
        parsed = JSON.parse(raw);
      } catch {
        // ignore
      }

      if (parsed && (parsed as any).ok === false) {
        const err = parsed as TelegramApiErr;
        const code = err.error_code ?? res.status;
        const desc = err.description ?? raw;
        throw new Error(`Telegram ${method} failed: ${code} ${desc}`);
      }

      if (!res.ok) throw new Error(`Telegram ${method} HTTP ${res.status}: ${raw}`);
      if (!parsed || (parsed as any).ok !== true) throw new Error(`Telegram ${method} bad response: ${raw}`);

      return (parsed as TelegramApiOk<T>).result;
    };

    return { status, retryAfterHeader, extract };
  });
}

// --- Teams support: bot identity + group invite links -----------------------

let _botMe: { id: number; username?: string; firstName?: string } | null = null;

// The bot's own identity (id + @username + display name), cached for the
// process. Used for team-invite deep links and for the group "is the bot being
// spoken to?" check (mention by @username or by name).
export async function telegramGetMe(): Promise<{
  id: number;
  username?: string;
  firstName?: string;
}> {
  if (_botMe) return _botMe;
  const me = await telegramApiCall<{ id: number; username?: string; first_name?: string }>(
    "getMe",
    {}
  );
  _botMe = { id: me.id, username: me.username, firstName: me.first_name };
  return _botMe;
}

// Create an invite link for a group/supergroup the bot administers. Requires the
// bot to be an admin with "invite users" permission; throws otherwise (caller
// falls back to a start-deep-link). `name` labels the link in Telegram's UI.
export async function telegramCreateInviteLink(
  chatId: string,
  opts?: { name?: string; memberLimit?: number }
): Promise<string> {
  const payload: Record<string, unknown> = { chat_id: chatId };
  if (opts?.name) payload.name = opts.name.slice(0, 32);
  if (opts?.memberLimit) payload.member_limit = opts.memberLimit;
  const res = await telegramApiCall<{ invite_link: string }>(
    "createChatInviteLink",
    payload
  );
  return res.invite_link;
}

export async function telegramSendChatAction(sessionId: string, action: TelegramChatAction): Promise<void> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const payload: any = { chat_id: chatId, action };
  if (threadId) payload.message_thread_id = threadId;

  const token = await resolveTelegramTokenForSession(sessionId);
  await telegramApiCall("sendChatAction", payload, token);
}

export function telegramStartChatActionLoop(
  sessionId: string,
  action: TelegramChatAction,
  opts?: { intervalMs?: number; shouldStop?: () => boolean | Promise<boolean> }
): { stop: () => void } {
  const intervalMs = Math.max(1000, Number(opts?.intervalMs ?? env("TELEGRAM_TYPING_INTERVAL_MS") ?? 4000));
  let stopped = false;

  (async () => {
    while (!stopped) {
      // Honor an external halt (e.g. /stop) so the typing indicator disappears
      // promptly even while a long turn is still running server-side.
      if (opts?.shouldStop) {
        try {
          if (await opts.shouldStop()) break;
        } catch {
          // treat a failed check as "keep going" — never get stuck stopped
        }
      }
      try {
        await telegramSendChatAction(sessionId, action);
      } catch {
        // best-effort
      }
      await new Promise<void>((r) => setTimeout(r, intervalMs));
    }
  })();

  return { stop: () => (stopped = true) };
}

// The fixed set of emoji Telegram allows as message reactions. Sending
// anything outside this list returns REACTION_INVALID, so we validate before
// calling. Source: Telegram Bot API available reactions.
export const TELEGRAM_ALLOWED_REACTIONS = [
  "👍", "👎", "❤", "🔥", "🥰", "👏", "😁", "🤔", "🤯", "😱", "🤬", "😢",
  "🎉", "🤩", "🙏", "👌", "🕊", "🤡", "🥱", "🥴", "😍", "🐳", "❤‍🔥", "🌚",
  "🌭", "💯", "🤣", "⚡", "🍌", "🏆", "💔", "🤨", "😐", "🍓", "🍾", "💋",
  "🖕", "😈", "😴", "😭", "🤓", "👻", "👨‍💻", "👀", "🎃", "🙈", "😇", "😨",
  "🤝", "✍", "🤗", "🫡", "🎅", "🎄", "☃", "💅", "🤪", "🗿", "🆒", "💘",
  "🙉", "🦄", "😘", "💊", "🙊", "😎", "👾", "🤷‍♂", "🤷", "🤷‍♀", "😡",
] as const;

// React to a specific user message with a single emoji. Best-effort: returns
// false on any failure instead of throwing, so a failed reaction never
// affects the actual conversation. `emoji` must be in TELEGRAM_ALLOWED_REACTIONS.
export async function telegramSetMessageReaction(
  sessionId: string,
  messageId: number,
  emoji: string,
  opts?: { big?: boolean }
): Promise<boolean> {
  const { chatId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId || !messageId) return false;
  if (!(TELEGRAM_ALLOWED_REACTIONS as readonly string[]).includes(emoji)) {
    return false;
  }
  try {
    const token = await resolveTelegramTokenForSession(sessionId);
    await telegramApiCall(
      "setMessageReaction",
      {
        chat_id: chatId,
        message_id: messageId,
        reaction: [{ type: "emoji", emoji }],
        is_big: opts?.big ?? false,
      },
      token
    );
    return true;
  } catch {
    return false;
  }
}

export async function telegramSendMessage(
  sessionId: string,
  text: string,
  opts?: { disableWebPreview?: boolean; disableNotification?: boolean }
): Promise<number> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const prepared = telegramFormatMessageHtml(text ?? "");
  const payload: any = {
    chat_id: chatId,
    text: prepared.html,
    parse_mode: prepared.parseMode,
    disable_web_page_preview: opts?.disableWebPreview ?? true,
    disable_notification: opts?.disableNotification ?? false,
  };
  if (threadId) payload.message_thread_id = threadId;

  const token = await resolveTelegramTokenForSession(sessionId);
  try {
    const result = await telegramApiCall<{ message_id: number }>("sendMessage", payload, token);
    return result.message_id;
  } catch (error) {
    if (!shouldRetryWithoutHtml(error)) throw error;

    const fallbackPayload: any = {
      ...payload,
      text: prepared.plain,
    };
    delete fallbackPayload.parse_mode;

    const result = await telegramApiCall<{ message_id: number }>("sendMessage", fallbackPayload, token);
    return result.message_id;
  }
}


function buildTelegramCaptionPayload(
  caption: string | undefined,
  disableNotification: boolean | undefined
): { caption?: string; parse_mode?: TelegramParseMode; disable_notification: boolean; plainCaption?: string } {
  if (!caption) {
    return {
      disable_notification: disableNotification ?? false,
    };
  }

  const prepared = telegramFormatMessageHtml(caption);
  return {
    caption: prepared.html,
    parse_mode: prepared.parseMode,
    disable_notification: disableNotification ?? false,
    plainCaption: prepared.plain,
  };
}

export async function telegramSendPhoto(
  sessionId: string,
  photo: string,
  opts?: { caption?: string; disableNotification?: boolean; hasSpoiler?: boolean }
): Promise<number> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const captionPayload = buildTelegramCaptionPayload(opts?.caption, opts?.disableNotification);
  const payload: any = {
    chat_id: chatId,
    photo,
    disable_notification: captionPayload.disable_notification,
    has_spoiler: opts?.hasSpoiler ?? false,
  };
  if (captionPayload.caption) {
    payload.caption = captionPayload.caption;
    payload.parse_mode = captionPayload.parse_mode;
  }
  if (threadId) payload.message_thread_id = threadId;

  const token = await resolveTelegramTokenForSession(sessionId);
  try {
    const result = await telegramApiCall<{ message_id: number }>("sendPhoto", payload, token);
    return result.message_id;
  } catch (error) {
    if (!shouldRetryWithoutHtml(error) || !captionPayload.plainCaption) throw error;

    const fallbackPayload: any = {
      ...payload,
      caption: captionPayload.plainCaption,
    };
    delete fallbackPayload.parse_mode;

    const result = await telegramApiCall<{ message_id: number }>("sendPhoto", fallbackPayload, token);
    return result.message_id;
  }
}

export async function telegramSendDocument(
  sessionId: string,
  document: string,
  opts?: { caption?: string; disableNotification?: boolean }
): Promise<number> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const captionPayload = buildTelegramCaptionPayload(opts?.caption, opts?.disableNotification);
  const payload: any = {
    chat_id: chatId,
    document,
    disable_notification: captionPayload.disable_notification,
  };
  if (captionPayload.caption) {
    payload.caption = captionPayload.caption;
    payload.parse_mode = captionPayload.parse_mode;
  }
  if (threadId) payload.message_thread_id = threadId;

  const token = await resolveTelegramTokenForSession(sessionId);
  try {
    const result = await telegramApiCall<{ message_id: number }>("sendDocument", payload, token);
    return result.message_id;
  } catch (error) {
    if (!shouldRetryWithoutHtml(error) || !captionPayload.plainCaption) throw error;

    const fallbackPayload: any = {
      ...payload,
      caption: captionPayload.plainCaption,
    };
    delete fallbackPayload.parse_mode;

    const result = await telegramApiCall<{ message_id: number }>("sendDocument", fallbackPayload, token);
    return result.message_id;
  }
}

export async function telegramSendAudio(
  sessionId: string,
  audio: TelegramMediaInput,
  opts?: {
    caption?: string;
    disableNotification?: boolean;
    title?: string;
    performer?: string;
    duration?: number;
    filename?: string;
  }
): Promise<number> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const captionPayload = buildTelegramCaptionPayload(opts?.caption, opts?.disableNotification);
  const form = new FormData();
  appendTelegramFormValue(form, "chat_id", chatId);
  appendTelegramMediaInput(form, "audio", audio, opts?.filename ?? "audio.mp3");
  appendTelegramFormValue(form, "disable_notification", captionPayload.disable_notification);
  appendTelegramFormValue(form, "title", opts?.title);
  appendTelegramFormValue(form, "performer", opts?.performer);
  appendTelegramFormValue(form, "duration", opts?.duration);
  if (threadId) appendTelegramFormValue(form, "message_thread_id", threadId);
  if (captionPayload.caption) {
    appendTelegramFormValue(form, "caption", captionPayload.caption);
    appendTelegramFormValue(form, "parse_mode", captionPayload.parse_mode);
  }

  const token = await resolveTelegramTokenForSession(sessionId);
  try {
    const result = await telegramApiCallFormData<{ message_id: number }>("sendAudio", form, token);
    return result.message_id;
  } catch (error) {
    if (!shouldRetryWithoutHtml(error) || !captionPayload.plainCaption) throw error;

    const fallbackForm = new FormData();
    appendTelegramFormValue(fallbackForm, "chat_id", chatId);
    appendTelegramMediaInput(fallbackForm, "audio", audio, opts?.filename ?? "audio.mp3");
    appendTelegramFormValue(fallbackForm, "disable_notification", captionPayload.disable_notification);
    appendTelegramFormValue(fallbackForm, "title", opts?.title);
    appendTelegramFormValue(fallbackForm, "performer", opts?.performer);
    appendTelegramFormValue(fallbackForm, "duration", opts?.duration);
    if (threadId) appendTelegramFormValue(fallbackForm, "message_thread_id", threadId);
    appendTelegramFormValue(fallbackForm, "caption", captionPayload.plainCaption);

    const result = await telegramApiCallFormData<{ message_id: number }>("sendAudio", fallbackForm, token);
    return result.message_id;
  }
}

export async function telegramSendVoice(
  sessionId: string,
  voice: TelegramMediaInput,
  opts?: {
    caption?: string;
    disableNotification?: boolean;
    duration?: number;
    filename?: string;
  }
): Promise<number> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const captionPayload = buildTelegramCaptionPayload(opts?.caption, opts?.disableNotification);
  const form = new FormData();
  appendTelegramFormValue(form, "chat_id", chatId);
  appendTelegramMediaInput(form, "voice", voice, opts?.filename ?? "voice.mp3");
  appendTelegramFormValue(form, "disable_notification", captionPayload.disable_notification);
  appendTelegramFormValue(form, "duration", opts?.duration);
  if (threadId) appendTelegramFormValue(form, "message_thread_id", threadId);
  if (captionPayload.caption) {
    appendTelegramFormValue(form, "caption", captionPayload.caption);
    appendTelegramFormValue(form, "parse_mode", captionPayload.parse_mode);
  }

  const token = await resolveTelegramTokenForSession(sessionId);
  try {
    const result = await telegramApiCallFormData<{ message_id: number }>("sendVoice", form, token);
    return result.message_id;
  } catch (error) {
    if (!shouldRetryWithoutHtml(error) || !captionPayload.plainCaption) throw error;

    const fallbackForm = new FormData();
    appendTelegramFormValue(fallbackForm, "chat_id", chatId);
    appendTelegramMediaInput(fallbackForm, "voice", voice, opts?.filename ?? "voice.mp3");
    appendTelegramFormValue(fallbackForm, "disable_notification", captionPayload.disable_notification);
    appendTelegramFormValue(fallbackForm, "duration", opts?.duration);
    if (threadId) appendTelegramFormValue(fallbackForm, "message_thread_id", threadId);
    appendTelegramFormValue(fallbackForm, "caption", captionPayload.plainCaption);

    const result = await telegramApiCallFormData<{ message_id: number }>("sendVoice", fallbackForm, token);
    return result.message_id;
  }
}

export async function telegramEditMessageText(
  sessionId: string,
  messageId: number,
  text: string,
  opts?: { disableWebPreview?: boolean }
): Promise<void> {
  const { chatId, threadId } = telegramSessionToChatAndThread(sessionId);
  if (!chatId) throw new Error(`Invalid telegram sessionId: ${sessionId}`);

  const prepared = telegramFormatMessageHtml(text ?? "");
  const payload: any = {
    chat_id: chatId,
    message_id: messageId,
    text: prepared.html,
    parse_mode: prepared.parseMode,
    disable_web_page_preview: opts?.disableWebPreview ?? true,
  };
  const token = await resolveTelegramTokenForSession(sessionId);
  try {
    await telegramApiCall("editMessageText", payload, token);
  } catch (error: any) {
    const msg = String(error?.message ?? "");
    if (msg.includes("message is not modified")) return;

    if (shouldRetryWithoutHtml(error)) {
      const fallbackPayload: any = {
        ...payload,
        text: prepared.plain,
      };
      delete fallbackPayload.parse_mode;

      try {
        await telegramApiCall("editMessageText", fallbackPayload, token);
        return;
      } catch (fallbackError: any) {
        const fallbackMsg = String(fallbackError?.message ?? "");
        if (fallbackMsg.includes("message is not modified")) return;
        throw fallbackError;
      }
    }

    throw error;
  }
}
