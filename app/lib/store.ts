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

  async get<T>(key: string): Promise<T | null> {
    return this.map.has(key) ? (this.map.get(key) as T) : null;
  }
  async set<T>(key: string, value: T, _opts?: { exSeconds?: number; nx?: boolean }): Promise<boolean> {
    this.map.set(key, value);
    return true;
  }
  async del(key: string): Promise<void> {
    this.map.delete(key);
    this.hmap.delete(key);
    this.zmap.delete(key);
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
    const h = this.hmap.get(key);
    if (!h) return null;
    return h.has(field) ? (h.get(field) as T) : null;
  }
  async hgetall<T>(key: string): Promise<Record<string, T>> {
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
    const z = this.zmap.get(key) ?? [];
    z.push({ score, member });
    z.sort((a, b) => a.score - b.score);
    this.zmap.set(key, z);
  }
  async zrangebyscore(key: string, min: number, max: number, opts?: { limit?: number }): Promise<string[]> {
    const z = this.zmap.get(key) ?? [];
    const filtered = z.filter((x) => x.score >= min && x.score <= max).map((x) => x.member);
    if (opts?.limit != null) return filtered.slice(0, opts.limit);
    return filtered;
  }
  async zrem(key: string, member: string): Promise<void> {
    const z = this.zmap.get(key) ?? [];
    this.zmap.set(key, z.filter((x) => x.member !== member));
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
}

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
  private async cmd<T = unknown>(args: (string | number)[]): Promise<T> {
    const res = await fetch(`${this.url}/pipeline`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${this.token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify([args]),
    });
    if (!res.ok) {
      const body = await res.text().catch(() => "");
      throw new Error(
        `Upstash /pipeline ${args[0]} → ${res.status} ${body.slice(0, 200)}`
      );
    }
    const json = (await res.json()) as Array<{ result?: T; error?: string }>;
    const first = Array.isArray(json) ? json[0] : (json as any);
    if (first?.error) {
      throw new Error(`Upstash cmd ${args[0]} → ${first.error}`);
    }
    return (first?.result ?? undefined) as T;
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
