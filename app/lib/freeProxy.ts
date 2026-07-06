// app/lib/freeProxy.ts
//
// Free-proxy fallback for the browser tool. Used when Webshare (the
// paid provider) is unavailable — e.g. the outage that prompted this
// file's creation. Fetches the iplocate/free-proxy-list US list and
// returns a Playwright-shaped config the same shape Webshare returns,
// so callers don't care which provider wins.
//
// Realistic expectations: most entries on a public free-proxy list are
// dead, slow, or rate-limited at any given moment. We try the same IP
// for the whole session (no per-request rotation — sites mistrust
// mid-session IP changes), and we DO NOT pre-validate (HEAD-checking
// every proxy on the list would add multiple seconds to every browse
// and a hot proxy can still die mid-request). The right mental model:
// this is degraded-service fallback, not a substitute for Webshare.
//
// Source: https://github.com/iplocate/free-proxy-list (refreshed daily).
// The format is one `ip:port` per line, optional comment lines that
// start with `#`.

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";
import type { PlaywrightProxyConfig } from "@/app/lib/webshareProxy";

const SOURCE_URL =
  "https://raw.githubusercontent.com/iplocate/free-proxy-list/main/countries/US/proxies.txt";

const CACHE_KEY = "freeproxy:us_list_v1";
// 1 hour TTL — the upstream list refreshes daily, but free proxies die
// fast and we don't want to keep retrying a known-dead one for too long.
const CACHE_TTL_SECONDS = 60 * 60;

const FETCH_DEADLINE_MS = 8_000;

function isEnabled(): boolean {
  // Hard-disable hatch: lets you bypass the free-proxy fallback entirely
  // when its proxies are causing more pain than they solve.
  if (env("BROWSER_FREE_PROXY_DISABLED") === "true") return false;
  // Default: enabled. The fallback only fires when Webshare returns
  // null, so this is non-disruptive when Webshare is healthy.
  return true;
}

type Endpoint = { host: string; port: number };

function parseList(text: string): Endpoint[] {
  const out: Endpoint[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const m = line.match(/^([^:\s]+):(\d{2,5})\b/);
    if (!m) continue;
    const port = Number.parseInt(m[2]!, 10);
    if (!Number.isFinite(port) || port < 1 || port > 65535) continue;
    out.push({ host: m[1]!, port });
  }
  return out;
}

async function fetchListWithDeadline(): Promise<Endpoint[]> {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), FETCH_DEADLINE_MS);
  try {
    const res = await fetch(SOURCE_URL, {
      signal: ac.signal,
      headers: { Accept: "text/plain" },
    });
    if (!res.ok) {
      throw new Error(`free-proxy fetch ${res.status}`);
    }
    const txt = await res.text();
    return parseList(txt);
  } finally {
    clearTimeout(timer);
  }
}

async function getList(): Promise<Endpoint[]> {
  if (!isEnabled()) return [];
  const store = getStore();
  try {
    const cached = await store.get<Endpoint[]>(CACHE_KEY);
    if (Array.isArray(cached) && cached.length > 0) return cached;
  } catch {
    // best-effort; cache miss is fine
  }
  try {
    const list = await fetchListWithDeadline();
    if (list.length === 0) return [];
    try {
      await store.set(CACHE_KEY, list, { exSeconds: CACHE_TTL_SECONDS });
    } catch {
      // best-effort; if Redis is unreachable just return the list
    }
    return list;
  } catch {
    // network error / abort — caller will fall through to direct egress
    return [];
  }
}

// Returns a Playwright-ready proxy config (or null when no proxies
// are available). Used as a fallback by webshareProxy.pickProxy when
// the paid provider returns null.
export async function pickFreeProxy(): Promise<PlaywrightProxyConfig | null> {
  const list = await getList();
  if (list.length === 0) return null;
  // Random pick — no health check. Sticky for the session by virtue of
  // the caller only invoking pickFreeProxy once per browse.
  const e = list[Math.floor(Math.random() * list.length)]!;
  return {
    server: `http://${e.host}:${e.port}`,
    // Free proxies on this list don't authenticate; pass empty creds.
    // Playwright handles this fine — it just skips the Proxy-Authorization
    // header.
    username: "",
    password: "",
  };
}

// Diagnostic for any future /proxies command that wants to surface the
// fallback pool too.
export async function summarizeFreeProxies(): Promise<
  Array<{ host: string; port: number }>
> {
  return getList();
}
