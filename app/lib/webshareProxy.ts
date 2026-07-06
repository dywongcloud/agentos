// app/lib/webshareProxy.ts
//
// Webshare residential / datacenter proxy integration.
//
// Why this exists: anti-bot services (Cloudflare, PerimeterX, Akamai) flag
// requests from known datacenter IPs — Vercel's outbound IPs included. A
// "stable Vercel IP" actually makes detection WORSE because the IP earns a
// known-bot reputation. Residential / mixed-geo proxies are the standard
// mitigation.
//
// What this does:
//   1. Pulls the tenant's proxy list from Webshare's API (cached in Redis
//      with a 30-minute TTL).
//   2. picks one randomly per browser session (per-session, not per-request
//      — many sites mistrust mid-session IP changes).
//   3. Returns a Playwright-shaped proxy config the browser tool plumbs into
//      `chromium.launch` / `newContext`.
//
// If WEBSHARE_API_KEY isn't set OR BROWSER_PROXY_ENABLED isn't "true",
// `pickProxy()` returns null and the browser runs against Vercel's egress
// IP — same behavior as before this module landed.

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";

const CACHE_KEY = "webshare:proxy_list_v2";
const CACHE_TTL_SECONDS = 30 * 60;
const DEFAULT_PAGE_SIZE = 50;
const API_BASE = "https://proxy.webshare.io/api/v2";

export type WebshareProxy = {
  id: string;
  username: string;
  password: string;
  proxy_address: string;
  port: number;
  valid: boolean;
  country_code: string;
  city_name?: string;
};

export type PlaywrightProxyConfig = {
  server: string;
  username: string;
  password: string;
  bypass?: string;
};

function isEnabled(): boolean {
  if (env("BROWSER_PROXY_ENABLED") !== "true") return false;
  // Two valid configurations:
  //   1. Rotating endpoint (preferred): one URL with sticky session, IP
  //      held stable for the session duration set on the Webshare side.
  //   2. Static list via API: many specific proxies, we pick one per browse.
  return (
    !!env("WEBSHARE_ROTATING_ENDPOINT") ||
    !!env("WEBSHARE_API_KEY")
  );
}

// When the rotating-endpoint env vars are set, we skip the API call entirely
// and return that endpoint directly. Cheaper, simpler, gives us 8-hour
// sticky sessions matched to a US geo by default (the user's existing
// Webshare config).
function getRotatingEndpoint(): PlaywrightProxyConfig | null {
  const server = env("WEBSHARE_ROTATING_ENDPOINT");
  const username = env("WEBSHARE_ROTATING_USERNAME");
  const password = env("WEBSHARE_ROTATING_PASSWORD");
  if (!server || !username || !password) return null;
  // Normalize: accept "p.webshare.io:80", "http://p.webshare.io:80", or
  // full URL with creds (we strip those).
  let normalized = server.trim();
  if (!/^https?:\/\//i.test(normalized)) {
    normalized = `http://${normalized}`;
  }
  // Strip any embedded credentials in the URL (we use the separate env vars).
  try {
    const u = new URL(normalized);
    u.username = "";
    u.password = "";
    normalized = u.toString().replace(/\/$/, "");
  } catch {
    // best-effort; pass through unchanged
  }
  return { server: normalized, username, password };
}

async function fetchProxyListFromApi(): Promise<WebshareProxy[]> {
  const key = env("WEBSHARE_API_KEY");
  if (!key) return [];

  const url = `${API_BASE}/proxy/list/?mode=direct&page_size=${DEFAULT_PAGE_SIZE}&valid=true`;
  const res = await fetch(url, {
    headers: {
      Authorization: `Token ${key}`,
      Accept: "application/json",
    },
  });
  if (!res.ok) {
    throw new Error(
      `webshare API ${res.status}: ${(await res.text().catch(() => "")).slice(0, 200)}`
    );
  }
  const body = (await res.json()) as { results?: WebshareProxy[] };
  return (body.results ?? []).filter((p) => p && p.valid);
}

export async function getProxyList(): Promise<WebshareProxy[]> {
  if (!isEnabled()) return [];
  const store = getStore();
  const cached = await store.get<WebshareProxy[]>(CACHE_KEY);
  if (Array.isArray(cached) && cached.length > 0) return cached;

  let list: WebshareProxy[] = [];
  try {
    list = await fetchProxyListFromApi();
  } catch {
    // Network or auth error — return empty so the browser falls back to
    // direct egress rather than hanging.
    return [];
  }

  if (list.length === 0) return [];
  await store.set(CACHE_KEY, list, { exSeconds: CACHE_TTL_SECONDS });
  return list;
}

// Pick a proxy for a browse session. Returns a Playwright-ready proxy config
// or null when proxying is disabled / no proxies available.
//
// Preference order:
//   1. EMERGENCY: BROWSER_PROXY_DISABLE_ALL=true → null (direct egress).
//   2. Webshare rotating-endpoint env (sticky session, geo-targeted).
//   3. Webshare static list via API (one chosen at random).
//   4. Free-proxy fallback (iplocate/free-proxy-list, US) — kicks in
//      when Webshare returns null AND the paid path was actually
//      attempted, so a Webshare outage stops the browse from running
//      direct-from-Vercel (which carries its own bot-flag risk) and
//      uses the free list instead. Unreliable; see app/lib/freeProxy.ts.
//   5. null (direct egress) — last resort.
export async function pickProxy(): Promise<PlaywrightProxyConfig | null> {
  // Hard-disable: force direct egress regardless of provider config.
  // Use when proxies of all kinds are causing more issues than they
  // solve (e.g. both Webshare AND the free list are down).
  if (env("BROWSER_PROXY_DISABLE_ALL") === "true") return null;

  if (isEnabled()) {
    const rotating = getRotatingEndpoint();
    if (rotating) return rotating;

    const list = await getProxyList();
    if (list.length > 0) {
      const p = list[Math.floor(Math.random() * list.length)];
      return {
        server: `http://${p.proxy_address}:${p.port}`,
        username: p.username,
        password: p.password,
      };
    }
  }

  // Webshare unavailable (disabled, API key missing, list empty, or
  // outage). Try the free-proxy fallback — degraded service but better
  // than running from Vercel's egress IP through bot-aware sites.
  try {
    const { pickFreeProxy } = await import("@/app/lib/freeProxy");
    const free = await pickFreeProxy();
    if (free) return free;
  } catch {
    // import or fetch error; fall through to direct egress
  }

  return null;
}

// Diagnostic — surface which proxies the system has, masked. Useful for the
// /proxies Telegram command (optional, not built in slice 6).
export async function summarizeProxies(): Promise<
  Array<{ host: string; country: string; city?: string }>
> {
  const list = await getProxyList();
  return list.map((p) => ({
    host: `${p.proxy_address}:${p.port}`,
    country: p.country_code,
    city: p.city_name,
  }));
}
