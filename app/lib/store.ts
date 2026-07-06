// app/lib/store.ts
import { env } from "@/app/lib/env";

export type JsonValue =
  | string
  | number
  | boolean
  | null
  | { [key: string]: JsonValue }
  | JsonValue[];

export interface Store {
  get<T = JsonValue>(key: string): Promise<T | null>;
  set<T = JsonValue>(key: string, value: T, opts?: { exSeconds?: number; nx?: boolean }): Promise<boolean>;
  del(key: string): Promise<void>;

  // Hash helpers
  hset<T = JsonValue>(key: string, field: string, value: T): Promise<void>;
  // Atomic set-if-absent: true iff the field was newly created. Lets a
  // read-modify-write over shared state (e.g. a team roster) become a single
  // race-free command instead of GET→mutate→SET.
  hsetnx<T = JsonValue>(key: string, field: string, value: T): Promise<boolean>;
  hget<T = JsonValue>(key: string, field: string): Promise<T | null>;
  hgetall<T = JsonValue>(key: string): Promise<Record<string, T>>;
  hdel(key: string, field: string): Promise<void>;

  // Sorted set helpers
  zadd(key: string, score: number, member: string): Promise<void>;
  zrangebyscore(key: string, min: number, max: number, opts?: { limit?: number }): Promise<string[]>;
  zrem(key: string, member: string): Promise<void>;

  // List helpers (left-push, right-trim convention: index 0 = newest)
  lpush(key: string, value: string): Promise<number>;
  lrange(key: string, start: number, stop: number): Promise<string[]>;
  llen(key: string): Promise<number>;
  ltrim(key: string, start: number, stop: number): Promise<void>;

  // Set helpers
  sadd(key: string, member: string): Promise<void>;
  smembers(key: string): Promise<string[]>;
  sismember(key: string, member: string): Promise<boolean>;
  srem(key: string, member: string): Promise<void>;
  scard(key: string): Promise<number>;

  // Pipeline helper: batch multiple Redis commands in a single HTTP round-trip.
  // Each element of cmds is a Redis command array (e.g. ["GET", "mykey"]).
  // Returns an array of decoded results in the same order as cmds.
  // Throws if any slot comes back with an error.
  pipelineMany(cmds: (string | number)[][]): Promise<unknown[]>;
}

function ensureEventTargetPolyfill() {
  const g: any = globalThis;

  if (typeof g.EventTarget === "undefined") {
    class MiniEventTarget {
      private _listeners = new Map<string, Set<(evt: any) => void>>();

      addEventListener(type: string, cb: (evt: any) => void) {
        if (!cb) return;
        const set = this._listeners.get(type) ?? new Set();
        set.add(cb);
        this._listeners.set(type, set);
      }

      removeEventListener(type: string, cb: (evt: any) => void) {
        this._listeners.get(type)?.delete(cb);
      }

      dispatchEvent(evt: any) {
        const type = evt?.type;
        if (!type) return true;
        const set = this._listeners.get(type);
        if (!set) return true;
        for (const cb of set) cb(evt);
        return true;
      }
    }

    g.EventTarget = MiniEventTarget;
  }

  if (typeof (globalThis as any).Event === "undefined") {
    (globalThis as any).Event = class {
      type: string;
      constructor(type: string) {
        this.type = type;
      }
    };
  }
}

class MemoryStore implements Store {
  private map = new Map<string, any>();
  private hmap = new Map<string, Map<string, any>>();
  private zmap = new Map<string, Array<{ score: number; member: string }>>();
  private lmap = new Map<string, string[]>();
  private smap = new Map<string, Set<string>>();
  // TTL emulation: absolute expiry timestamps (ms) set by SET ... EX / EXPIRE.
  // Learn-subsystem keys (learn:attr:*, learn:graph:*) lean on TTL as their
  // bounded-growth mechanism, so this fallback store needs to actually honor
  // it rather than silently keeping every key forever.
  private expiresAt = new Map<string, number>();

  private prune(key: string): void {
    const exp = this.expiresAt.get(key);
    if (exp !== undefined && exp <= Date.now()) {
      this.map.delete(key);
      this.hmap.delete(key);
      this.zmap.delete(key);
      this.lmap.delete(key);
      this.smap.delete(key);
      this.expiresAt.delete(key);
    }
  }

  async get<T>(key: string): Promise<T | null> {
    this.prune(key);
    return this.map.has(key) ? (this.map.get(key) as T) : null;
  }
  async set<T>(key: string, value: T, opts?: { exSeconds?: number; nx?: boolean }): Promise<boolean> {
    this.prune(key);
    if (opts?.nx && this.map.has(key)) return false;
    this.map.set(key, value);
    if (opts?.exSeconds) this.expiresAt.set(key, Date.now() + opts.exSeconds * 1000);
    else this.expiresAt.delete(key);
    return true;
  }
  async del(key: string): Promise<void> {
    this.map.delete(key);
    this.hmap.delete(key);
    this.zmap.delete(key);
    this.lmap.delete(key);
    this.smap.delete(key);
    this.expiresAt.delete(key);
  }
  async expire(key: string, seconds: number): Promise<void> {
    this.prune(key);
    this.expiresAt.set(key, Date.now() + seconds * 1000);
  }

  async hset<T>(key: string, field: string, value: T): Promise<void> {
    const h = this.hmap.get(key) ?? new Map<string, any>();
    h.set(field, value);
    this.hmap.set(key, h);
  }
  async hsetnx<T>(key: string, field: string, value: T): Promise<boolean> {
    const h = this.hmap.get(key) ?? new Map<string, any>();
    if (h.has(field)) return false;
    h.set(field, value);
    this.hmap.set(key, h);
    return true;
  }
  async hget<T>(key: string, field: string): Promise<T | null> {
    this.prune(key);
    const h = this.hmap.get(key);
    if (!h) return null;
    return h.has(field) ? (h.get(field) as T) : null;
  }
  async hgetall<T>(key: string): Promise<Record<string, T>> {
    this.prune(key);
    const h = this.hmap.get(key);
    if (!h) return {};
    return Object.fromEntries(h.entries()) as Record<string, T>;
  }
  async hdel(key: string, field: string): Promise<void> {
    const h = this.hmap.get(key);
    if (!h) return;
    h.delete(field);
  }

  async zadd(key: string, score: number, member: string): Promise<void> {
    this.prune(key);
    const z = this.zmap.get(key) ?? [];
    const existing = z.find((x) => x.member === member);
    if (existing) existing.score = score;
    else z.push({ score, member });
    z.sort((a, b) => a.score - b.score);
    this.zmap.set(key, z);
  }
  async zrangebyscore(key: string, min: number, max: number, opts?: { limit?: number }): Promise<string[]> {
    this.prune(key);
    const z = this.zmap.get(key) ?? [];
    const filtered = z.filter((x) => x.score >= min && x.score <= max).map((x) => x.member);
    if (opts?.limit != null) return filtered.slice(0, opts.limit);
    return filtered;
  }
  async zrem(key: string, member: string): Promise<void> {
    const z = this.zmap.get(key) ?? [];
    this.zmap.set(key, z.filter((x) => x.member !== member));
  }
  // Redis ZSCORE: single member's score, or null if absent.
  async zscore(key: string, member: string): Promise<number | null> {
    this.prune(key);
    const z = this.zmap.get(key) ?? [];
    const hit = z.find((x) => x.member === member);
    return hit ? hit.score : null;
  }
  // Redis ZRANGE by rank (ascending score order), with optional WITHSCORES
  // flattening ([member, score, member, score, ...]) to mirror the wire
  // format the Upstash REST client returns.
  async zrange(key: string, start: number, stop: number, withScores: boolean): Promise<(string | number)[]> {
    this.prune(key);
    const z = this.zmap.get(key) ?? [];
    const end = stop < 0 ? z.length + stop + 1 : stop + 1;
    const slice = z.slice(start < 0 ? Math.max(0, z.length + start) : start, end);
    if (!withScores) return slice.map((x) => x.member);
    return slice.flatMap((x) => [x.member, x.score]);
  }
  // Redis ZREMRANGEBYRANK: drop members whose ascending-score rank falls in
  // [start, stop] (inclusive, negative indices count from the highest rank).
  async zremrangebyrank(key: string, start: number, stop: number): Promise<void> {
    const z = this.zmap.get(key) ?? [];
    const end = stop < 0 ? z.length + stop + 1 : stop + 1;
    const from = start < 0 ? Math.max(0, z.length + start) : start;
    const kept = z.filter((_, i) => i < from || i >= end);
    this.zmap.set(key, kept);
  }

  async lpush(key: string, value: string): Promise<number> {
    const l = this.lmap.get(key) ?? [];
    l.unshift(value);
    this.lmap.set(key, l);
    return l.length;
  }
  async lrange(key: string, start: number, stop: number): Promise<string[]> {
    const l = this.lmap.get(key) ?? [];
    const end = stop < 0 ? l.length + stop + 1 : stop + 1;
    return l.slice(start < 0 ? Math.max(0, l.length + start) : start, end);
  }
  async llen(key: string): Promise<number> {
    return (this.lmap.get(key) ?? []).length;
  }
  async ltrim(key: string, start: number, stop: number): Promise<void> {
    const l = this.lmap.get(key) ?? [];
    const end = stop < 0 ? l.length + stop + 1 : stop + 1;
    this.lmap.set(key, l.slice(start < 0 ? Math.max(0, l.length + start) : start, end));
  }

  async sadd(key: string, member: string): Promise<void> {
    const s = this.smap.get(key) ?? new Set<string>();
    s.add(member);
    this.smap.set(key, s);
  }
  async smembers(key: string): Promise<string[]> {
    return Array.from(this.smap.get(key) ?? []);
  }
  async sismember(key: string, member: string): Promise<boolean> {
    return this.smap.get(key)?.has(member) ?? false;
  }
  async srem(key: string, member: string): Promise<void> {
    this.smap.get(key)?.delete(member);
  }
  async scard(key: string): Promise<number> {
    return (this.smap.get(key) ?? new Set()).size;
  }

  // Sequential fallback: MemoryStore has no network, so run each cmd in order.
  async pipelineMany(cmds: (string | number)[][]): Promise<unknown[]> {
    const results: unknown[] = [];
    for (const cmd of cmds) {
      const [op, ...rest] = cmd;
      // Dispatch to the appropriate typed helper by command name.
      switch (String(op).toUpperCase()) {
        case "GET":        results.push(await this.get(String(rest[0]))); break;
        case "SET": {
          const [k, v, ...flags] = rest;
          const opts: { exSeconds?: number; nx?: boolean } = {};
          for (let i = 0; i < flags.length; i++) {
            if (String(flags[i]).toUpperCase() === "EX") opts.exSeconds = Number(flags[++i]);
            if (String(flags[i]).toUpperCase() === "NX") opts.nx = true;
          }
          results.push(await this.set(String(k), v, opts));
          break;
        }
        case "DEL":        await this.del(String(rest[0])); results.push(null); break;
        case "EXPIRE":     await this.expire(String(rest[0]), Number(rest[1])); results.push(1); break;
        case "HSET":       await this.hset(String(rest[0]), String(rest[1]), rest[2]); results.push(null); break;
        case "HSETNX":     results.push(await this.hsetnx(String(rest[0]), String(rest[1]), rest[2])); break;
        case "HGET":       results.push(await this.hget(String(rest[0]), String(rest[1]))); break;
        case "HGETALL":    results.push(await this.hgetall(String(rest[0]))); break;
        case "HDEL":       await this.hdel(String(rest[0]), String(rest[1])); results.push(null); break;
        case "ZADD":       await this.zadd(String(rest[0]), Number(rest[1]), String(rest[2])); results.push(null); break;
        case "ZRANGEBYSCORE": results.push(await this.zrangebyscore(String(rest[0]), Number(rest[1]), Number(rest[2]))); break;
        case "ZREM":       await this.zrem(String(rest[0]), String(rest[1])); results.push(null); break;
        case "ZSCORE":     results.push(await this.zscore(String(rest[0]), String(rest[1]))); break;
        case "ZRANGE": {
          const [k, start, stop, ...flags] = rest;
          const withScores = flags.some((f) => String(f).toUpperCase() === "WITHSCORES");
          results.push(await this.zrange(String(k), Number(start), Number(stop), withScores));
          break;
        }
        case "ZREMRANGEBYRANK":
          await this.zremrangebyrank(String(rest[0]), Number(rest[1]), Number(rest[2]));
          results.push(null);
          break;
        case "LPUSH":      results.push(await this.lpush(String(rest[0]), String(rest[1]))); break;
        case "LRANGE":     results.push(await this.lrange(String(rest[0]), Number(rest[1]), Number(rest[2]))); break;
        case "LLEN":       results.push(await this.llen(String(rest[0]))); break;
        case "LTRIM":      await this.ltrim(String(rest[0]), Number(rest[1]), Number(rest[2])); results.push(null); break;
        case "SADD":       await this.sadd(String(rest[0]), String(rest[1])); results.push(null); break;
        case "SMEMBERS":   results.push(await this.smembers(String(rest[0]))); break;
        case "SISMEMBER":  results.push(await this.sismember(String(rest[0]), String(rest[1]))); break;
        case "SREM":       await this.srem(String(rest[0]), String(rest[1])); results.push(null); break;
        case "SCARD":      results.push(await this.scard(String(rest[0]))); break;
        default:
          throw new Error(`MemoryStore.pipelineMany: unsupported command "${op}"`);
      }
    }
    return results;
  }
}

// Statuses that are transient infrastructure failures and worth retrying.
const RETRYABLE_STATUSES = new Set([429, 500, 502, 503, 504]);
// Max attempts = 1 initial + 3 retries.
const MAX_ATTEMPTS = 4;
// Base backoff in ms; delay = BASE * 4^attempt (100ms, 400ms, 1600ms).
const BACKOFF_BASE_MS = 100;

/**
 * Direct-REST Upstash store.
 *
 * We do NOT use the @upstash/redis SDK here. Inside Vercel Workflow `"use step"`
 * bundles, the SDK's auto-pipelining silently drops writes (returns optimistic
 * predicted results without ever calling the server). Setting
 * enableAutoPipelining: false didn't reliably fix it. Talking to Upstash REST
 * directly via fetch() avoids the entire issue and works identically in
 * route handlers, steps, and the workflow VM (though we never call from the
 * workflow VM — see jobSteps).
 */
class UpstashStore implements Store {
  private url: string;
  private token: string;

  constructor() {
    const url = env("KV_REST_API_URL") ?? env("UPSTASH_REDIS_REST_URL");
    const token = env("KV_REST_API_TOKEN") ?? env("UPSTASH_REDIS_REST_TOKEN");
    if (!url || !token) {
      throw new Error(
        "Missing Redis env vars. Set KV_REST_API_URL/TOKEN or UPSTASH_REDIS_REST_URL/TOKEN."
      );
    }
    this.url = url.replace(/\/$/, "");
    this.token = token;
  }

  // Run a single Redis command via Upstash REST /pipeline endpoint.
  // /pipeline returns the response as a JSON ARRAY of { result } / { error },
  // NOT wrapped in an outer { result }. Don't try to share a generic envelope
  // helper with the path-style endpoints — they have a different shape.
  //
  // Retries up to 3 times (4 total attempts) on HTTP 429/500/502/503/504 with
  // exponential backoff: 100ms, 400ms, 1600ms. Does not retry on 401/403/404.
  private async cmd<T = unknown>(args: (string | number)[]): Promise<T> {
    let lastError: Error | undefined;

    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
      if (attempt > 0) {
        // Exponential backoff: 100ms * 4^(attempt-1)
        const delay = BACKOFF_BASE_MS * Math.pow(4, attempt - 1);
        await new Promise((resolve) => setTimeout(resolve, delay));
      }

      const res = await fetch(`${this.url}/pipeline`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${this.token}`,
          "Content-Type": "application/json",
          Connection: "keep-alive",
        },
        body: JSON.stringify([args]),
      });

      if (!res.ok) {
        const body = await res.text().catch(() => "");
        const err = new Error(
          `Upstash /pipeline ${args[0]} → ${res.status} ${body.slice(0, 200)}`
        );
        if (RETRYABLE_STATUSES.has(res.status)) {
          lastError = err;
          continue; // retry
        }
        throw err; // non-retryable (e.g. 401, 403, 404)
      }

      const json = (await res.json()) as Array<{ result?: T; error?: string }>;
      const first = Array.isArray(json) ? json[0] : (json as any);
      if (first?.error) {
        throw new Error(`Upstash cmd ${args[0]} → ${first.error}`);
      }
      return (first?.result ?? undefined) as T;
    }

    throw lastError ?? new Error(`Upstash /pipeline ${args[0]} failed after ${MAX_ATTEMPTS} attempts`);
  }

  // Encoding rule: strings are stored as-is (no JSON quoting), everything
  // else is JSON.stringify'd. Matches @upstash/redis automaticDeserialization
  // behavior and keeps debug flags / tokens / IDs human-readable in Redis.
  private encode(value: unknown): string {
    return typeof value === "string" ? value : JSON.stringify(value);
  }

  // Decoding rule (must mirror encode): only JSON.parse when the value looks
  // like a JSON structure — object `{...}`, array `[...]`, or quoted string
  // `"..."`. A bare `"1"` is a raw user string (e.g. the debug flag), NOT
  // the JSON-encoded number 1; parsing it would silently break `=== "1"`
  // comparisons everywhere downstream. Same for raw tokens, ISO dates,
  // numeric strings, etc.
  //
  // Bug fixed by this change: isDebugMode read "1" back as the number 1,
  // returned false, and the entire streaming UX was silently disabled even
  // with /debug on. Same bug applied to /stop, pairing codes, and any
  // store.get<string> path holding a digit-only or boolean-shaped value.
  private decode<T>(raw: unknown): T | null {
    if (raw === null || raw === undefined) return null;
    if (typeof raw !== "string") return raw as T;
    const t = raw.length > 0 ? raw[0] : "";
    const looksJson = t === "{" || t === "[" || t === "\"";
    if (!looksJson) {
      return raw as unknown as T;
    }
    try {
      return JSON.parse(raw) as T;
    } catch {
      return raw as unknown as T;
    }
  }

  async get<T>(key: string): Promise<T | null> {
    const v = await this.cmd<string | null>(["GET", key]);
    return this.decode<T>(v);
  }

  async set<T>(
    key: string,
    value: T,
    opts?: { exSeconds?: number; nx?: boolean }
  ): Promise<boolean> {
    const args: (string | number)[] = ["SET", key, this.encode(value)];
    if (opts?.exSeconds && opts.exSeconds > 0) args.push("EX", opts.exSeconds);
    if (opts?.nx) args.push("NX");
    const res = await this.cmd<string | null>(args);
    return res === "OK";
  }

  async del(key: string): Promise<void> {
    await this.cmd<number>(["DEL", key]);
  }

  async hset<T>(key: string, field: string, value: T): Promise<void> {
    await this.cmd<number>(["HSET", key, field, this.encode(value)]);
  }
  async hsetnx<T>(key: string, field: string, value: T): Promise<boolean> {
    // HSETNX returns 1 iff the field was newly set — atomic set-if-absent.
    return (await this.cmd<number>(["HSETNX", key, field, this.encode(value)])) === 1;
  }
  async hget<T>(key: string, field: string): Promise<T | null> {
    const v = await this.cmd<string | null>(["HGET", key, field]);
    return this.decode<T>(v);
  }
  async hgetall<T>(key: string): Promise<Record<string, T>> {
    // /pipeline HGETALL returns a flat [field, value, field, value, …] array.
    const flat = await this.cmd<unknown[] | null>(["HGETALL", key]);
    const out: Record<string, T> = {};
    if (Array.isArray(flat)) {
      for (let i = 0; i + 1 < flat.length; i += 2) {
        out[String(flat[i])] = this.decode<T>(flat[i + 1] as string) as T;
      }
    }
    return out;
  }
  async hdel(key: string, field: string): Promise<void> {
    await this.cmd<number>(["HDEL", key, field]);
  }

  async zadd(key: string, score: number, member: string): Promise<void> {
    await this.cmd<number>(["ZADD", key, score, member]);
  }
  async zrangebyscore(
    key: string,
    min: number,
    max: number,
    opts?: { limit?: number }
  ): Promise<string[]> {
    // Redis ZRANGEBYSCORE wants the literals "-inf"/"+inf" for unbounded
    // ranges. ±Infinity as a JS number would JSON.stringify to `null` over
    // the Upstash REST /pipeline body, which Redis rejects ("min or max is
    // not a float") and 500s the whole eval API. Translate explicitly.
    const fmt = (n: number): string | number =>
      n === Number.NEGATIVE_INFINITY
        ? "-inf"
        : n === Number.POSITIVE_INFINITY
          ? "+inf"
          : n;
    const args: (string | number)[] = [
      "ZRANGEBYSCORE",
      key,
      fmt(min),
      fmt(max),
    ];
    if (opts?.limit != null) args.push("LIMIT", 0, opts.limit);
    const v = await this.cmd<string[] | null>(args);
    return Array.isArray(v) ? v.map(String) : [];
  }
  async zrem(key: string, member: string): Promise<void> {
    await this.cmd<number>(["ZREM", key, member]);
  }

  async lpush(key: string, value: string): Promise<number> {
    return (await this.cmd<number>(["LPUSH", key, this.encode(value)])) ?? 0;
  }
  async lrange(key: string, start: number, stop: number): Promise<string[]> {
    const v = await this.cmd<string[] | null>(["LRANGE", key, start, stop]);
    return Array.isArray(v) ? v.map(String) : [];
  }
  async llen(key: string): Promise<number> {
    return (await this.cmd<number>(["LLEN", key])) ?? 0;
  }
  async ltrim(key: string, start: number, stop: number): Promise<void> {
    await this.cmd<string>(["LTRIM", key, start, stop]);
  }

  async sadd(key: string, member: string): Promise<void> {
    await this.cmd<number>(["SADD", key, member]);
  }
  async smembers(key: string): Promise<string[]> {
    const v = await this.cmd<string[] | null>(["SMEMBERS", key]);
    return Array.isArray(v) ? v.map(String) : [];
  }
  async sismember(key: string, member: string): Promise<boolean> {
    return (await this.cmd<number>(["SISMEMBER", key, member])) === 1;
  }
  async srem(key: string, member: string): Promise<void> {
    await this.cmd<number>(["SREM", key, member]);
  }
  async scard(key: string): Promise<number> {
    return (await this.cmd<number>(["SCARD", key])) ?? 0;
  }

  // Batch multiple Redis commands in a single HTTP POST to /pipeline.
  // Returns decoded results in slot order; throws on any per-slot error.
  async pipelineMany(cmds: (string | number)[][]): Promise<unknown[]> {
    if (cmds.length === 0) return [];

    let lastError: Error | undefined;

    for (let attempt = 0; attempt < MAX_ATTEMPTS; attempt++) {
      if (attempt > 0) {
        const delay = BACKOFF_BASE_MS * Math.pow(4, attempt - 1);
        await new Promise((resolve) => setTimeout(resolve, delay));
      }

      const res = await fetch(`${this.url}/pipeline`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${this.token}`,
          "Content-Type": "application/json",
          Connection: "keep-alive",
        },
        body: JSON.stringify(cmds),
      });

      if (!res.ok) {
        const body = await res.text().catch(() => "");
        const err = new Error(
          `Upstash /pipeline (batch ${cmds.length}) → ${res.status} ${body.slice(0, 200)}`
        );
        if (RETRYABLE_STATUSES.has(res.status)) {
          lastError = err;
          continue;
        }
        throw err;
      }

      const json = (await res.json()) as Array<{ result?: unknown; error?: string }>;

      // Check every slot for errors before returning anything.
      for (let i = 0; i < json.length; i++) {
        const slot = json[i];
        if (slot?.error) {
          throw new Error(
            `Upstash pipelineMany slot ${i} (${cmds[i]?.[0]}) → ${slot.error}`
          );
        }
      }

      return json.map((slot) => this.decode(slot?.result ?? null));
    }

    throw lastError ?? new Error(`Upstash /pipeline batch failed after ${MAX_ATTEMPTS} attempts`);
  }
}

let _store: Store | null = null;

export function getStore(): Store {
  if (_store) return _store;
  try {
    _store = new UpstashStore();
  } catch {
    _store = new MemoryStore();
  }
  return _store;
}
