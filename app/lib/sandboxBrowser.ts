// app/lib/sandboxBrowser.ts
//
// Headless browser via Vercel Sandbox.
//
// Architecture:
//   1. A persistent named sandbox ("claw-browser") hosts a pre-installed
//      Playwright + Chromium environment.
//   2. The agent-side `browse_web` tool sends (goal, start_url) to the
//      sandbox, which runs a Node script that drives Chromium with OpenAI's
//      `computer-use-preview` model in a screenshot → action loop.
//   3. The script returns a JSON summary back to the tool.
//
// Why a persistent sandbox: installing Playwright + downloading Chromium
// costs ~60-90s the first time. Reusing the sandbox between calls cuts
// subsequent invocations to a few seconds plus the actual browsing time.
//
// Why `computer-use-preview`: it's OpenAI's purpose-built model for driving
// a screen — handles click coordinates, scroll, typing, page-rendered state
// far better than asking a general model to plan low-level Playwright calls.

import { Sandbox } from "@vercel/sandbox";

import { env } from "@/app/lib/env";
import {
  loadStorageState,
  saveStorageState,
  type StorageState,
} from "@/app/lib/browserAuthStore";
import { pickProxy, type PlaywrightProxyConfig } from "@/app/lib/webshareProxy";
import { enrichBrowseGoal } from "@/app/lib/browserBrain";

const SANDBOX_NAME = "claw-browser";
// /tmp is universally writable inside the Vercel Sandbox container.
const SANDBOX_DIR = "/tmp/claw-browser";
const BROWSE_SCRIPT_PATH = `${SANDBOX_DIR}/browse.cjs`;
const STATE_DIR = `${SANDBOX_DIR}/states`;
// v7 = + @anthropic-ai/claude-code installed alongside agent-browser. The
// sandbox now hosts BOTH the headless browser AND a Claude Code instance
// the agent can delegate engineering tasks to.
const INIT_MARKER_PATH = `${SANDBOX_DIR}/.initialized.v7`;

// System libraries Chromium needs at runtime. List taken verbatim from
// vercel-labs/agent-browser examples/environments/lib/agent-browser-sandbox.ts
// (Amazon Linux package names — Vercel Sandbox base image).
const CHROMIUM_SYSTEM_DEPS = [
  "nss",
  "nspr",
  "libxkbcommon",
  "atk",
  "at-spi2-atk",
  "at-spi2-core",
  "libXcomposite",
  "libXdamage",
  "libXrandr",
  "libXfixes",
  "libXcursor",
  "libXi",
  "libXtst",
  "libXScrnSaver",
  "libXext",
  "mesa-libgbm",
  "libdrm",
  "mesa-libGL",
  "mesa-libEGL",
  "cups-libs",
  "alsa-lib",
  "pango",
  "cairo",
  "gtk3",
  "dbus-libs",
];

// Idle timeout for the persistent sandbox. After this period without
// commands, the sandbox terminates and the next call cold-starts (re-installs
// Playwright + Chromium). Tuned generously so chat-driven browsing doesn't
// pay the cold-start cost on every message.
const SANDBOX_IDLE_TIMEOUT_MS = 60 * 60 * 1000; // 1 hour

// Per-browse-call wall-clock cap. The CUA loop also has its own action
// budget; this is the outer cap. Vercel function timeout is ~13min on Pro;
// we stay well under.
const BROWSE_TIMEOUT_MS = 3 * 60 * 1000; // 3 minutes

export type BrowseRequest = {
  goal: string;
  startUrl?: string;
  maxActions?: number;
  // Optional: jobId so the script can stamp screenshots into the job VFS
  // later. Slice 5 doesn't use this; reserved.
  jobId?: string;
  // Tenant id for auth state lookup. When set, the tenant's persisted
  // cookies/localStorage are loaded into Chromium before navigation and the
  // updated state is harvested back when the browse completes.
  tenantId?: string;
  // When true (default for the browse_web tool), the goal is first expanded
  // by the Gemini 3.1 Pro side-car into a multi-step navigation plan before
  // it's handed to the computer-use driver. Set false for deterministic
  // flows like login where a fixed goal is better.
  enrich?: boolean;
};

export type BrowseResult = {
  ok: boolean;
  result: string;
  finalUrl?: string;
  actionsTaken?: number;
  hitCap?: boolean;
  error?: string;
  storageState?: StorageState;
};

export type LoginRequest = {
  loginUrl: string;
  username: string;
  password: string;
  twoFaCode?: string;
  // Selector hints — usually unnecessary because we use computer-use-preview
  // to find fields visually, but useful for stubborn sites.
  selectors?: {
    username?: string;
    password?: string;
    submit?: string;
  };
  tenantId: string;
};

export type LoginResult = {
  ok: boolean;
  finalUrl?: string;
  signedInIndicator?: string;
  error?: string;
};

// --- the script that runs INSIDE the sandbox --------------------------------
// We embed it as a string and `writeFiles` it during sandbox init. Kept in a
// single .cjs blob so the sandbox doesn't need a bundler.
// LEGACY computer-use-preview script preserved for diff context; the new
// agent-browser-based script is below at AGENT_BROWSER_SCRIPT and used by
// browseWeb / loginToSite.
const _LEGACY_BROWSE_SCRIPT = String.raw`/* eslint-disable */
// Node script executed inside Vercel Sandbox. Drives Chromium via
// playwright-core + @sparticuz/chromium (serverless-friendly binary) using
// OpenAI's computer-use-preview model.

const { chromium: pwChromium } = require("playwright-core");
const sparticuz = require("@sparticuz/chromium");
const OpenAI = require("openai");

const VIEWPORT = { width: 1280, height: 800 };

async function main() {
  const argsJson = process.argv[2] || "{}";
  let args;
  try {
    args = JSON.parse(argsJson);
  } catch {
    output({ ok: false, result: "", error: "Bad args JSON" });
    process.exit(1);
  }
  const {
    goal,
    startUrl,
    maxActions = 15,
    wallClockMs = 90000,
    storageState,
    proxy,
  } = args || {};
  if (!goal) {
    output({ ok: false, result: "", error: "Missing 'goal'" });
    process.exit(1);
  }

  const openai = new OpenAI();

  // Stealth-ish launch flags merged with @sparticuz/chromium's defaults
  // (which include sandbox-friendly flags for serverless environments).
  const customArgs = [
    "--disable-blink-features=AutomationControlled",
    "--disable-features=IsolateOrigins,site-per-process",
  ];
  const launchArgs = [...sparticuz.args, ...customArgs];

  const executablePath = await sparticuz.executablePath();

  const launchOpts = {
    args: launchArgs,
    defaultViewport: sparticuz.defaultViewport,
    executablePath,
    headless: true,
  };
  // proxy applies at the BROWSER level so all contexts inherit it. The
  // wrapper picks one Webshare residential proxy per session.
  if (args && args.proxy && args.proxy.server) {
    launchOpts.proxy = args.proxy;
  }
  const browser = await pwChromium.launch(launchOpts);

  // Realistic user-agent. Chrome version should be reasonably current so
  // sites' UA-based feature checks don't trip; bump when noticing breakage.
  const REAL_UA =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

  // storageState is a Playwright JSON blob (cookies + localStorage). When the
  // outer wrapper has saved auth for this tenant, sites the agent visits will
  // come up already logged in.
  const contextOpts = {
    viewport: VIEWPORT,
    userAgent: REAL_UA,
    locale: "en-US",
    timezoneId: "America/New_York",
  };
  if (storageState && typeof storageState === "object") {
    contextOpts.storageState = storageState;
  }
  const context = await browser.newContext(contextOpts);

  // Last-mile fingerprint scrub. computer-use-preview moves the mouse and
  // types but doesn't think about JS-level signals — these are what
  // playwright-extra/stealth would handle:
  await context.addInitScript(() => {
    // navigator.webdriver lie
    Object.defineProperty(navigator, "webdriver", { get: () => false });
    // Plugins length (headless Chrome typically reports 0)
    Object.defineProperty(navigator, "plugins", {
      get: () => [1, 2, 3, 4, 5],
    });
    // Languages
    Object.defineProperty(navigator, "languages", {
      get: () => ["en-US", "en"],
    });
  });

  const page = await context.newPage();

  const start = Date.now();
  let actionsTaken = 0;
  let lastCallId = null;
  let previousResponseId = null;
  let finalText = "";
  let hitCap = false;

  try {
    await page.goto(startUrl || "https://www.google.com", {
      waitUntil: "domcontentloaded",
      timeout: 30000,
    });

    for (let i = 0; i < maxActions; i++) {
      if (Date.now() - start > wallClockMs) {
        hitCap = true;
        break;
      }

      const shot = await page.screenshot({ type: "png", fullPage: false });
      const dataUrl = "data:image/png;base64," + shot.toString("base64");

      const request = {
        model: "computer-use-preview",
        tools: [
          {
            type: "computer_use_preview",
            display_width: VIEWPORT.width,
            display_height: VIEWPORT.height,
            environment: "browser",
          },
        ],
        truncation: "auto",
      };

      if (i === 0) {
        request.input = [
          {
            role: "user",
            content: [
              {
                type: "input_text",
                text:
                  "Goal: " +
                  goal +
                  "\n\nUse the visible browser to accomplish the goal. When done, respond with the final answer or relevant information found.",
              },
              { type: "input_image", image_url: dataUrl },
            ],
          },
        ];
      } else {
        request.previous_response_id = previousResponseId;
        request.input = [
          {
            type: "computer_call_output",
            call_id: lastCallId,
            output: { type: "input_image", image_url: dataUrl },
          },
        ];
      }

      let response;
      try {
        response = await openai.responses.create(request);
      } catch (err) {
        // If computer-use-preview isn't accessible on this account, surface
        // a clear error to the caller instead of looping.
        output({
          ok: false,
          result: "",
          error:
            "computer-use-preview API error: " +
            (err && err.message ? err.message : String(err)),
        });
        await browser.close();
        process.exit(1);
      }

      previousResponseId = response.id;
      const items = response.output || [];
      const computerCalls = items.filter((o) => o.type === "computer_call");
      const messages = items.filter((o) => o.type === "message");

      if (computerCalls.length === 0) {
        finalText = messages
          .map((m) => {
            const parts = m.content || [];
            return parts
              .map((p) => (p.text || p.output_text || ""))
              .join(" ");
          })
          .join("\n")
          .trim();
        break;
      }

      const call = computerCalls[0];
      lastCallId = call.call_id;
      await executeAction(page, call.action);
      actionsTaken++;

      // Tiny settle delay so the page has time to update before the next
      // screenshot. Tunable.
      await page.waitForTimeout(400);
    }

    if (!finalText) {
      hitCap = true;
      const url = page.url();
      const bodyText = await page
        .evaluate(() => (document.body && document.body.innerText) || "")
        .catch(() => "");
      finalText =
        "Hit action/time cap. Final URL: " +
        url +
        "\n\nPage content excerpt:\n" +
        (bodyText || "(no text)").slice(0, 4000);
    }

    // Harvest the updated storageState so the wrapper can persist any new
    // cookies / localStorage changes (e.g. if the agent logged in mid-browse).
    let updatedState = null;
    try {
      updatedState = await context.storageState();
    } catch {}

    output({
      ok: true,
      result: finalText,
      finalUrl: page.url(),
      actionsTaken,
      hitCap,
      storageState: updatedState,
    });
  } catch (err) {
    let updatedState = null;
    try {
      updatedState = await context.storageState();
    } catch {}
    output({
      ok: false,
      result: "",
      error: (err && err.message) || String(err),
      finalUrl: page.url(),
      actionsTaken,
      storageState: updatedState,
    });
  } finally {
    try {
      await browser.close();
    } catch {}
  }
}

async function executeAction(page, action) {
  if (!action || !action.type) return;
  switch (action.type) {
    case "click":
      await page.mouse.click(action.x, action.y, {
        button: action.button || "left",
      });
      break;
    case "double_click":
      await page.mouse.dblclick(action.x, action.y);
      break;
    case "type":
      await page.keyboard.type(action.text || "");
      break;
    case "keypress":
      for (const k of action.keys || []) {
        await page.keyboard.press(mapKey(k));
      }
      break;
    case "scroll":
      await page.mouse.move(action.x || 0, action.y || 0);
      await page.mouse.wheel(action.scroll_x || 0, action.scroll_y || 0);
      break;
    case "wait":
      await page.waitForTimeout(1000);
      break;
    case "move":
      await page.mouse.move(action.x || 0, action.y || 0);
      break;
    case "screenshot":
      // The next loop iteration takes a screenshot anyway.
      break;
    case "drag": {
      const path = action.path || [];
      if (path.length < 2) break;
      await page.mouse.move(path[0].x, path[0].y);
      await page.mouse.down();
      for (let i = 1; i < path.length; i++) {
        await page.mouse.move(path[i].x, path[i].y, { steps: 5 });
      }
      await page.mouse.up();
      break;
    }
    default:
      // Unknown action — ignore.
      break;
  }
}

function mapKey(k) {
  // OpenAI uses some name variants; Playwright accepts Enter, Tab, etc.
  const t = String(k).toLowerCase();
  const map = {
    return: "Enter",
    enter: "Enter",
    tab: "Tab",
    esc: "Escape",
    escape: "Escape",
    space: " ",
    spacebar: " ",
    backspace: "Backspace",
    delete: "Delete",
    up: "ArrowUp",
    down: "ArrowDown",
    left: "ArrowLeft",
    right: "ArrowRight",
    home: "Home",
    end: "End",
    pageup: "PageUp",
    pagedown: "PageDown",
  };
  return map[t] || k;
}

function output(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

main().catch((err) => {
  output({ ok: false, result: "", error: (err && err.message) || String(err) });
  process.exit(1);
});
`;

// =============================================================================
// New script: drives vercel-labs/agent-browser CLI for browser primitives and
// runs the agentic action loop using gpt-5.4-mini with structured tool calls.
// Cheaper than computer-use-preview screenshot-driving, more deterministic
// (refs from the accessibility tree are stable across renders), and avoids
// the Chromium-deps headache because agent-browser bundles a working binary.
// =============================================================================
const AGENT_BROWSER_SCRIPT = String.raw`/* eslint-disable */
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const OpenAI = require("openai");

const MAX_REF_LIST = 25;
const SNAPSHOT_DEPTH = 6;

function ab(args, opts) {
  const res = spawnSync("agent-browser", args, {
    encoding: "utf8",
    maxBuffer: 25 * 1024 * 1024,
    ...(opts || {}),
  });
  if (res.error) throw res.error;
  if (res.status !== 0) {
    throw new Error(
      "agent-browser " + args.join(" ") + " (exit " + res.status + "): " +
        (res.stderr || res.stdout || "").slice(-400)
    );
  }
  return res.stdout;
}

function abJson(args) {
  const out = ab([...args, "--json"]);
  // agent-browser prints non-JSON status lines on some commands. The last
  // line is the JSON envelope.
  const lines = out.trim().split(/\r?\n/).filter(Boolean);
  const lastLine = lines[lines.length - 1];
  try {
    return JSON.parse(lastLine);
  } catch (e) {
    return { success: false, data: {}, raw: out };
  }
}

function buildProxyArg(proxy) {
  if (!proxy || !proxy.server) return null;
  let s = proxy.server.replace(/\/$/, "");
  if (proxy.username) {
    const u = new URL(s);
    u.username = encodeURIComponent(proxy.username);
    u.password = encodeURIComponent(proxy.password || "");
    s = u.toString().replace(/\/$/, "");
  }
  return s;
}

async function decideNextAction(openai, model, goal, snapshot, history) {
  const tools = [
    {
      type: "function",
      function: {
        name: "click_element",
        description:
          "Click an element by its accessibility ref (e.g. 'e1', 'e2') from the latest snapshot.",
        parameters: {
          type: "object",
          properties: { ref: { type: "string" } },
          required: ["ref"],
          additionalProperties: false,
        },
      },
    },
    {
      type: "function",
      function: {
        name: "fill_element",
        description:
          "Fill a text input by ref with the given text. Clears the field first.",
        parameters: {
          type: "object",
          properties: {
            ref: { type: "string" },
            text: { type: "string" },
          },
          required: ["ref", "text"],
          additionalProperties: false,
        },
      },
    },
    {
      type: "function",
      function: {
        name: "press_key",
        description: "Press a keyboard key (Enter, Tab, Escape, etc.).",
        parameters: {
          type: "object",
          properties: { key: { type: "string" } },
          required: ["key"],
          additionalProperties: false,
        },
      },
    },
    {
      type: "function",
      function: {
        name: "scroll",
        description: "Scroll the page up or down by ~600px.",
        parameters: {
          type: "object",
          properties: {
            direction: { type: "string", enum: ["up", "down"] },
          },
          required: ["direction"],
          additionalProperties: false,
        },
      },
    },
    {
      type: "function",
      function: {
        name: "navigate",
        description: "Navigate the current tab to a URL.",
        parameters: {
          type: "object",
          properties: { url: { type: "string" } },
          required: ["url"],
          additionalProperties: false,
        },
      },
    },
    {
      type: "function",
      function: {
        name: "wait_for",
        description:
          "Wait for text to appear on the page (useful after navigations / form submits).",
        parameters: {
          type: "object",
          properties: {
            text: { type: "string" },
            timeout_ms: { type: "number" },
          },
          required: ["text"],
          additionalProperties: false,
        },
      },
    },
    {
      type: "function",
      function: {
        name: "finish",
        description:
          "Conclude the browse session. Pass the final user-facing answer or a status indicator (LOGIN_OK / TWOFA_REQUIRED / LOGIN_REJECTED / LOGIN_BLOCKED).",
        parameters: {
          type: "object",
          properties: { result: { type: "string" } },
          required: ["result"],
          additionalProperties: false,
        },
      },
    },
  ];

  const system =
    "You drive a web browser to accomplish the user's goal. Each turn you see (1) the current URL, (2) the page title, (3) the visible body text, (4) the accessibility tree of interactive elements with refs like @e1, @e2. Pick ONE next action via a tool call. " +
    "When the goal is achieved OR you cannot proceed further, call finish() with a concise user-facing summary. " +
    "If the page has NO interactive elements (refs list is empty) and the body text already answers the goal — e.g. an IP-check page that just shows the IP — call finish() with the answer extracted from the body text. Do not waste actions scrolling or clicking. " +
    "Never make up refs that aren't in the snapshot. Use snapshot refs (@e1, @e2) for click/fill, not CSS selectors.";

  const refsList = Object.entries(snapshot.refs || {});
  const refsBlock = refsList.length
    ? refsList
        .slice(0, MAX_REF_LIST)
        .map(
          ([k, v]) =>
            "@" +
            k +
            " " +
            (v.role || "?") +
            " " +
            JSON.stringify(v.name || "")
        )
        .join("\n")
    : "(no interactive refs on this page)";

  const bodySnippet = (snapshot.bodyText || "").trim().slice(0, 3000);

  const userPrompt =
    "Goal: " +
    goal +
    "\n\nCurrent URL: " +
    snapshot.url +
    "\nPage title: " +
    (snapshot.title || "(none)") +
    "\n\nVisible page text:\n" +
    (bodySnippet || "(empty)") +
    "\n\nInteractive elements:\n" +
    refsBlock +
    "\n\nAccessibility tree (compact):\n" +
    (snapshot.tree || "").slice(0, 4000) +
    (history.length
      ? "\n\nRecent actions:\n" +
        history
          .slice(-6)
          .map((h, i) => i + 1 + ". " + h)
          .join("\n")
      : "");

  const resp = await openai.chat.completions.create({
    model,
    tools,
    tool_choice: "required",
    temperature: 0.2,
    messages: [
      { role: "system", content: system },
      { role: "user", content: userPrompt },
    ],
  });

  const call = resp.choices[0]?.message?.tool_calls?.[0];
  if (!call) {
    return { fn: "finish", args: { result: resp.choices[0]?.message?.content || "" } };
  }
  let parsed;
  try {
    parsed = JSON.parse(call.function.arguments || "{}");
  } catch {
    parsed = {};
  }
  return { fn: call.function.name, args: parsed };
}

function executeAction(action) {
  const refArg = (r) => (r && !r.startsWith("@") ? "@" + r : r);
  switch (action.fn) {
    case "click_element":
      return ab(["click", refArg(action.args.ref)]);
    case "fill_element":
      return ab(["fill", refArg(action.args.ref), action.args.text]);
    case "press_key":
      return ab(["press", action.args.key]);
    case "scroll":
      return ab(["scroll", action.args.direction, "600"]);
    case "navigate":
      return ab(["open", action.args.url]);
    case "wait_for":
      return ab(["wait", "--text", action.args.text]);
    default:
      throw new Error("unknown action: " + action.fn);
  }
}

async function main() {
  const argsJson = process.argv[2] || "{}";
  let inArgs;
  try {
    inArgs = JSON.parse(argsJson);
  } catch (e) {
    output({ ok: false, result: "", error: "Bad args JSON" });
    process.exit(1);
  }
  const {
    goal,
    startUrl,
    maxActions = 12,
    wallClockMs = 120000,
    storageState,
    proxy,
    sessionFile,
  } = inArgs || {};
  if (!goal) {
    output({ ok: false, result: "", error: "Missing goal" });
    process.exit(1);
  }

  const model = process.env.AGENT_BROWSER_DECIDER_MODEL || "gpt-5.4-mini";
  const openai = new OpenAI();

  // Write the storageState (if provided) to a file the CLI can load via --state.
  let stateInputPath = null;
  if (storageState && typeof storageState === "object") {
    stateInputPath = sessionFile || "/tmp/claw-browser/states/_in.json";
    try {
      fs.writeFileSync(stateInputPath, JSON.stringify(storageState));
    } catch (e) {
      stateInputPath = null;
    }
  }

  const proxyUrl = buildProxyArg(proxy);

  // Open the browser with proxy + state if available.
  const openCmd = ["open"];
  if (proxyUrl) openCmd.push("--proxy", proxyUrl);
  if (stateInputPath) openCmd.push("--state", stateInputPath);
  // Realistic UA + content boundaries help the model understand the page.
  openCmd.push(
    "--user-agent",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
  );
  if (startUrl) openCmd.push(startUrl);

  const start = Date.now();
  let actionsTaken = 0;
  let finalText = "";
  let hitCap = false;
  let lastUrl = "";
  let savedState = null;

  try {
    ab(openCmd);

    const history = [];
    for (let i = 0; i < maxActions; i++) {
      if (Date.now() - start > wallClockMs) {
        hitCap = true;
        break;
      }

      // Pull the accessibility tree (interactive elements only, compact)
      // AND the plain page text. Many "lookup" sites (whoami / IP check /
      // status pages) have no interactive elements at all — refs is empty
      // and the model would call finish() with nothing if we only sent the
      // tree. Including the visible body text lets it answer from the page.
      let snapshot;
      try {
        const snapJson = abJson([
          "snapshot",
          "-i",
          "-c",
          "-d",
          String(SNAPSHOT_DEPTH),
        ]);
        const url = ab(["get", "url"]).trim();
        lastUrl = url;
        let title = "";
        try {
          title = ab(["get", "title"]).trim();
        } catch {}
        let bodyText = "";
        try {
          // get text body works as a CSS selector for <body>. Output is the
          // visible text content, perfect for plain pages.
          const bodyJson = abJson(["get", "text", "body"]);
          bodyText = (bodyJson.data?.text || "").toString();
        } catch {}
        snapshot = {
          tree: snapJson.data?.snapshot || "",
          refs: snapJson.data?.refs || {},
          url,
          title,
          bodyText,
        };
      } catch (e) {
        snapshot = {
          tree: "(snapshot failed: " + (e.message || e) + ")",
          refs: {},
          url: lastUrl,
          title: "",
          bodyText: "",
        };
      }

      const decision = await decideNextAction(openai, model, goal, snapshot, history);

      if (decision.fn === "finish") {
        finalText = decision.args.result || "";
        break;
      }

      try {
        executeAction(decision);
        history.push(decision.fn + " " + JSON.stringify(decision.args));
        actionsTaken++;
      } catch (e) {
        history.push("(failed) " + decision.fn + ": " + (e.message || e));
        // continue; the next snapshot will reflect the error state and the
        // model can choose a different action.
      }

      // Small settle delay so DOM updates land before the next snapshot.
      await new Promise((r) => setTimeout(r, 300));
    }

    if (!finalText) {
      hitCap = true;
      try {
        const url = ab(["get", "url"]).trim();
        lastUrl = url;
        const title = ab(["get", "title"]).trim();
        finalText =
          "Hit action/time cap. URL: " +
          url +
          ", title: " +
          title +
          ". Actions taken: " +
          actionsTaken;
      } catch {
        finalText = "Hit cap; could not read final state.";
      }
    }

    // Save updated storage state for the wrapper to persist.
    const stateOutputPath =
      sessionFile || "/tmp/claw-browser/states/_out.json";
    try {
      ab(["state", "save", stateOutputPath]);
      savedState = JSON.parse(fs.readFileSync(stateOutputPath, "utf8"));
    } catch (e) {
      savedState = null;
    }

    output({
      ok: true,
      result: finalText,
      finalUrl: lastUrl,
      actionsTaken,
      hitCap,
      storageState: savedState,
    });
  } catch (err) {
    let savedState = null;
    try {
      const stateOutputPath =
        sessionFile || "/tmp/claw-browser/states/_out.json";
      ab(["state", "save", stateOutputPath]);
      savedState = JSON.parse(fs.readFileSync(stateOutputPath, "utf8"));
    } catch {}
    output({
      ok: false,
      result: "",
      error: (err && err.message) || String(err),
      finalUrl: lastUrl,
      actionsTaken,
      storageState: savedState,
    });
  } finally {
    try {
      ab(["close"]);
    } catch {}
  }
}

function output(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

main().catch((err) => {
  output({ ok: false, result: "", error: (err && err.message) || String(err) });
  process.exit(1);
});
`;

// ---------------------------------------------------------------------------

let sandboxPromise: Promise<Sandbox> | null = null;

async function getOrCreateBrowserSandbox(): Promise<Sandbox> {
  if (sandboxPromise) {
    try {
      return await sandboxPromise;
    } catch {
      sandboxPromise = null;
    }
  }
  sandboxPromise = (async () => {
    const oidcPresent = !!process.env.VERCEL_OIDC_TOKEN;
    const teamPresent =
      !!process.env.VERCEL_TEAM_ID || !!process.env.VERCEL_PROJECT_ID;
    console.log(
      `[sandboxBrowser] getOrCreate begin: oidc=${oidcPresent} team_or_project=${teamPresent}`
    );

    let sandbox: Sandbox;
    try {
      // vCPU count drives RAM: Vercel Sandbox docs say 2048 MB per vCPU.
      // Default 2 (= 4 GB) is comfortable for Chromium + Node builds; bump
      // via SANDBOX_VCPUS when running heavy tasks (build pipelines,
      // language servers, etc.). Coding-agent-template uses 4.
      const vcpuRaw = Number(env("SANDBOX_VCPUS") ?? "2");
      const vcpus = Number.isFinite(vcpuRaw) && vcpuRaw >= 1 ? Math.min(8, vcpuRaw) : 2;

      sandbox = await Sandbox.getOrCreate({
        name: SANDBOX_NAME,
        runtime: "node24",
        persistent: true,
        timeout: SANDBOX_IDLE_TIMEOUT_MS,
        resources: { vcpus },
        env: {
          OPENAI_API_KEY: env("OPENAI_API_KEY") ?? "",
          ANTHROPIC_API_KEY: env("ANTHROPIC_API_KEY") ?? "",
          PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: "0",
        },
      });
    } catch (err: any) {
      console.error(
        `[sandboxBrowser] Sandbox.getOrCreate FAILED: ${err?.message ?? String(err)} | stack: ${(err?.stack ?? "").slice(0, 600)}`
      );
      throw err;
    }
    console.log(`[sandboxBrowser] Sandbox.getOrCreate ok, probing init marker`);

    const probe = await sandbox.runCommand("test", ["-f", INIT_MARKER_PATH]);
    console.log(`[sandboxBrowser] init marker probe exitCode=${probe.exitCode}`);
    if (probe.exitCode !== 0) {
      console.log(`[sandboxBrowser] cold start: installing playwright + chromium`);

      // Diagnostic: identify the sandbox OS once so we know what installer
      // syntax to use. We've observed Amazon Linux 2023 in practice.
      const osRelease = await sandbox.runCommand("bash", [
        "-lc",
        "cat /etc/os-release 2>/dev/null || uname -a",
      ]);
      const osText = await osRelease.stdout();
      console.log(`[sandboxBrowser] OS info:\n${osText.slice(0, 400)}`);

      const mkdir = await sandbox.runCommand("mkdir", ["-p", SANDBOX_DIR]);
      if (mkdir.exitCode !== 0) {
        const err = await mkdir.stderr();
        throw new Error(
          `mkdir ${SANDBOX_DIR} failed (exit ${mkdir.exitCode}): ${err.slice(-300)}`
        );
      }

      // Step 1: install Chromium system deps via dnf (Amazon Linux base image).
      // List + commands copied from vercel-labs/agent-browser's reference
      // example; --skip-broken so a single missing pkg doesn't abort the rest.
      const sysdepsCmd =
        `sudo dnf clean all 2>&1 && ` +
        `sudo dnf install -y --skip-broken ${CHROMIUM_SYSTEM_DEPS.join(" ")} 2>&1 && ` +
        `sudo ldconfig 2>&1`;
      const sysdeps = await sandbox.runCommand("sh", ["-c", sysdepsCmd]);
      const sysdepsStdout = await sysdeps.stdout();
      console.log(
        `[sandboxBrowser] dnf install Chromium deps exitCode=${sysdeps.exitCode} tail=${sysdepsStdout.slice(-600)}`
      );
      if (sysdeps.exitCode !== 0) {
        throw new Error(
          `dnf install chromium deps failed (exit ${sysdeps.exitCode}): ${sysdepsStdout.slice(-400)}`
        );
      }

      // Step 2: install agent-browser CLI + claude-code CLI + openai SDK
      // (local). claude-code is the official Anthropic CLI; we run it with
      // --print --dangerously-skip-permissions so the agentOS can delegate
      // engineering tasks without per-tool approval prompts.
      const npmInstall = await sandbox.runCommand("sh", [
        "-c",
        `cd ${SANDBOX_DIR} && ` +
          `npm init -y >/dev/null 2>&1 && ` +
          `npm install -g agent-browser @anthropic-ai/claude-code 2>&1 && ` +
          `npm install --no-audit --no-fund openai 2>&1 && ` +
          `mkdir -p ${STATE_DIR} ${SANDBOX_DIR}/cc-workdirs 2>&1`,
      ]);
      const npmOut = await npmInstall.stdout();
      console.log(
        `[sandboxBrowser] npm install exitCode=${npmInstall.exitCode} tail=${npmOut.slice(-500)}`
      );
      if (npmInstall.exitCode !== 0) {
        throw new Error(
          `npm install failed (exit ${npmInstall.exitCode}): ${npmOut.slice(-500)}`
        );
      }

      // Step 3: download Chrome-for-Testing binary the CLI uses.
      const chromeInstall = await sandbox.runCommand("sh", [
        "-c",
        `npx agent-browser install 2>&1`,
      ]);
      const chromeOut = await chromeInstall.stdout();
      console.log(
        `[sandboxBrowser] agent-browser install exitCode=${chromeInstall.exitCode} tail=${chromeOut.slice(-500)}`
      );
      if (chromeInstall.exitCode !== 0) {
        throw new Error(
          `agent-browser install failed (exit ${chromeInstall.exitCode}): ${chromeOut.slice(-500)}`
        );
      }

      await sandbox.writeFiles([
        {
          path: BROWSE_SCRIPT_PATH,
          content: Buffer.from(AGENT_BROWSER_SCRIPT, "utf8"),
        },
      ]);
      await sandbox.runCommand("touch", [INIT_MARKER_PATH]);
      console.log(`[sandboxBrowser] cold start init complete`);
    } else {
      // Always overwrite the script so code changes in this file propagate
      // without nuking the Playwright install.
      await sandbox.writeFiles([
        {
          path: BROWSE_SCRIPT_PATH,
          content: Buffer.from(AGENT_BROWSER_SCRIPT, "utf8"),
        },
      ]);
      console.log(`[sandboxBrowser] warm sandbox, script refreshed`);
    }

    return sandbox;
  })();
  return sandboxPromise;
}

export async function browseWeb(req: BrowseRequest): Promise<BrowseResult> {
  if (!env("OPENAI_API_KEY")) {
    return {
      ok: false,
      result: "",
      error: "OPENAI_API_KEY not configured",
    };
  }

  let sandbox: Sandbox;
  try {
    sandbox = await getOrCreateBrowserSandbox();
  } catch (err: any) {
    return {
      ok: false,
      result: "",
      error: `sandbox bootstrap failed: ${err?.message ?? String(err)}`,
    };
  }

  // Load persisted auth state for this tenant. Sites the agent visits will
  // come up already logged in. Falls back gracefully if no state or no
  // encryption key configured.
  let storageState: StorageState | undefined;
  if (req.tenantId) {
    try {
      storageState = await loadStorageState(req.tenantId);
    } catch {
      storageState = undefined;
    }
  }

  // Residential proxy selection. Null when proxying is disabled or no
  // proxies are available; the sandbox script falls through to direct
  // egress. Per-session proxy assignment (one IP for the whole browse).
  let proxy: PlaywrightProxyConfig | null = null;
  try {
    proxy = await pickProxy();
  } catch {
    proxy = null;
  }

  // Gemini 3.1 Pro side-car: expand the goal into a multi-step navigation
  // plan before the computer-use driver sees it. Best-effort + non-breaking —
  // on any failure or when disabled, enrichBrowseGoal returns the raw goal.
  // Wrap in a deadline so a hung Gemini call can't block the browse from
  // even starting.
  let effectiveGoal = req.goal;
  let effectiveStartUrl = req.startUrl;
  if (req.enrich !== false) {
    try {
      const ENRICH_DEADLINE_MS = 20_000;
      const enrichDeadline = new Promise<never>((_, reject) => {
        setTimeout(
          () => reject(new Error(`enrich timeout ${ENRICH_DEADLINE_MS}ms`)),
          ENRICH_DEADLINE_MS
        );
      });
      const plan = await Promise.race([
        enrichBrowseGoal({
          goal: req.goal,
          startUrl: req.startUrl,
        }),
        enrichDeadline,
      ]);
      effectiveGoal = plan.enrichedGoal;
      effectiveStartUrl = plan.suggestedStartUrl ?? req.startUrl;
      console.log(
        `[sandboxBrowser] sidecar enriched=${plan.enriched} model=${plan.model} steps=${plan.steps.length}`
      );
    } catch (err: any) {
      console.warn(
        `[sandboxBrowser] sidecar enrichment threw, using raw goal: ${err?.message ?? String(err)}`
      );
    }
  }

  const payload = JSON.stringify({
    goal: effectiveGoal,
    startUrl: effectiveStartUrl ?? null,
    maxActions: req.maxActions ?? 15,
    wallClockMs: BROWSE_TIMEOUT_MS,
    storageState: storageState ?? null,
    proxy: proxy ?? null,
  });

  try {
    console.log(`[sandboxBrowser] runCommand browse.cjs (goal len=${req.goal.length}, startUrl=${req.startUrl ?? "(none)"})`);
    // The browse script's own wall-clock cap is BROWSE_TIMEOUT_MS (3 min).
    // Give the outer runCommand + stream reads a generous margin on top
    // so a normal-finish browse never trips the host-side guard, but a
    // hung sandbox connection (the "Stream ended before command finished"
    // class of failure we saw on /code) surfaces a clean error here
    // instead of hanging until Vercel kills the function instance.
    const HOST_DEADLINE_MS = BROWSE_TIMEOUT_MS + 90_000;
    const withDeadline = async <T>(p: Promise<T>, label: string): Promise<T> => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const deadline = new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`timeout after ${Math.round(HOST_DEADLINE_MS / 1000)}s: ${label}`)),
          HOST_DEADLINE_MS
        );
      });
      try {
        return await Promise.race([p, deadline]);
      } finally {
        if (timer) clearTimeout(timer);
      }
    };
    const result = await withDeadline(
      sandbox.runCommand("node", [BROWSE_SCRIPT_PATH, payload]),
      "browse.cjs runCommand"
    );
    let stdout: string;
    let stderr: string;
    try {
      stdout = await withDeadline(result.stdout(), "browse stdout");
      stderr = await withDeadline(result.stderr(), "browse stderr");
    } catch (e: any) {
      try {
        const k = (result as any).kill;
        if (typeof k === "function") await k.call(result);
      } catch {
        /* best-effort */
      }
      return {
        ok: false,
        result: "",
        error: `browse stream hang (${e?.message ?? String(e)}). The sandbox may have lost its connection mid-browse; retry usually works.`,
      };
    }
    console.log(
      `[sandboxBrowser] runCommand exitCode=${result.exitCode} stdout-tail=${stdout.slice(-300)} stderr-tail=${stderr.slice(-300)}`
    );
    if (result.exitCode !== 0) {
      return {
        ok: false,
        result: "",
        error: `sandbox exit ${result.exitCode}: ${stderr.slice(0, 500) || stdout.slice(0, 500)}`,
      };
    }

    // The script writes a single JSON line. Take the last line in case
    // anything else printed.
    const lines = stdout.trim().split(/\r?\n/).filter(Boolean);
    const lastLine = lines[lines.length - 1] ?? "{}";
    let parsed: BrowseResult;
    try {
      parsed = JSON.parse(lastLine) as BrowseResult;
    } catch (err: any) {
      return {
        ok: false,
        result: "",
        error: `bad sandbox JSON output: ${err?.message ?? String(err)} | raw: ${stdout.slice(0, 300)}`,
      };
    }

    // Harvest any updated cookies / localStorage back into the tenant's
    // encrypted store. We persist even on a failed browse — a partial login
    // (e.g. before a 2FA prompt) may still produce cookies worth keeping.
    if (req.tenantId && parsed.storageState && env("AUTH_STATE_ENCRYPTION_KEY")) {
      try {
        await saveStorageState(req.tenantId, parsed.storageState);
      } catch {
        // best-effort; don't fail the browse if the save errors
      }
    }

    // Don't leak the full storageState back to the AI SDK tool result — it's
    // sensitive and noisy. The save already happened above.
    delete parsed.storageState;
    return parsed;
  } catch (err: any) {
    return {
      ok: false,
      result: "",
      error: `runCommand failed: ${err?.message ?? String(err)}`,
    };
  }
}

// Drive a login flow by reusing the browse path with a tightly-scripted
// goal. computer-use-preview reads the screenshots, finds the username /
// password fields, types the values, submits, and harvests cookies via the
// same storageState pipeline that `browse_web` uses.
//
// If the site requests 2FA, the model is told to STOP and report "2FA
// required" — the caller (loginToSiteTool) surfaces that to the user, the
// user replies with the code, the tool re-invokes login with twoFaCode.
export async function loginToSite(req: LoginRequest): Promise<LoginResult> {
  const lines: string[] = [
    `Log in to this website on behalf of the user.`,
    `Open the login URL, find the username and password fields, type the credentials, and submit.`,
    `If everything works and you reach a logged-in page, conclude with: "LOGIN_OK".`,
    `If the site asks for a verification code (2FA / OTP) and no code is provided below, conclude with: "TWOFA_REQUIRED".`,
    `If the site rejects the credentials, conclude with: "LOGIN_REJECTED".`,
    `If you get stuck (captcha, rate limit, error page), conclude with: "LOGIN_BLOCKED: <one-line reason>".`,
    ``,
    `Credentials:`,
    `  username: ${req.username}`,
    `  password: ${req.password}`,
  ];
  if (req.twoFaCode) {
    lines.push(`  2FA code: ${req.twoFaCode}`);
  }
  if (req.selectors?.username || req.selectors?.password || req.selectors?.submit) {
    lines.push(``);
    lines.push(`Selector hints (only use if you can't see the field visually):`);
    if (req.selectors?.username) lines.push(`  username field: ${req.selectors.username}`);
    if (req.selectors?.password) lines.push(`  password field: ${req.selectors.password}`);
    if (req.selectors?.submit) lines.push(`  submit button: ${req.selectors.submit}`);
  }

  const goal = lines.join("\n");

  const result = await browseWeb({
    goal,
    startUrl: req.loginUrl,
    maxActions: 12,
    tenantId: req.tenantId,
    // Login is a fixed, deterministic script with sentinel outputs
    // (LOGIN_OK / TWOFA_REQUIRED / …) — don't let the planner rewrite it.
    enrich: false,
  });

  const txt = (result.result ?? "").toUpperCase();
  if (txt.includes("LOGIN_OK")) {
    return { ok: true, finalUrl: result.finalUrl, signedInIndicator: "LOGIN_OK" };
  }
  if (txt.includes("TWOFA_REQUIRED")) {
    return {
      ok: false,
      finalUrl: result.finalUrl,
      error: "2FA required",
      signedInIndicator: "TWOFA_REQUIRED",
    };
  }
  if (txt.includes("LOGIN_REJECTED")) {
    return {
      ok: false,
      finalUrl: result.finalUrl,
      error: "Site rejected credentials",
      signedInIndicator: "LOGIN_REJECTED",
    };
  }
  if (txt.includes("LOGIN_BLOCKED")) {
    return {
      ok: false,
      finalUrl: result.finalUrl,
      error: result.result,
      signedInIndicator: "LOGIN_BLOCKED",
    };
  }
  // Unclear — surface raw text.
  return {
    ok: false,
    finalUrl: result.finalUrl,
    error: result.error ?? `unclear outcome: ${(result.result ?? "").slice(0, 200)}`,
  };
}
