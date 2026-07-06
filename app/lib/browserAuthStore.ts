// app/lib/browserAuthStore.ts
//
// Encrypted store for per-tenant browser auth state.
//
// What's stored: Playwright's `storageState` JSON — cookies + localStorage
// for every site the agent has been logged into for this tenant. One blob
// per tenant; the blob contains entries for many hostnames.
//
// Encryption: AES-256-GCM via Web Crypto (workflow-VM-safe — no node:crypto
// import that WDK's static analyzer would reject). Key from env
// AUTH_STATE_ENCRYPTION_KEY, a 64-char hex string (32 bytes).
//
// Threat model (be honest):
//   - Encrypted-at-rest in Redis. Anyone with the env key can decrypt.
//     This is server-side encryption only — not end-to-end. For high-stakes
//     accounts (banking, primary email) use OAuth-based tools via Composio
//     instead of this raw cookie store.
//   - The cookies grant the same access the original browser had. Don't
//     enable this for accounts you wouldn't share with the operator of this
//     deployment.

import { env } from "@/app/lib/env";
import { getStore } from "@/app/lib/store";

// Playwright's storageState shape — keeping this loose because we don't
// import @playwright/test here (it'd bloat the function bundle). The
// sandbox-side script handles the real type.
export type StorageState = {
  cookies?: Array<{
    name: string;
    value: string;
    domain: string;
    path: string;
    expires?: number;
    httpOnly?: boolean;
    secure?: boolean;
    sameSite?: "Strict" | "Lax" | "None";
  }>;
  origins?: Array<{
    origin: string;
    localStorage?: Array<{ name: string; value: string }>;
  }>;
};

const EMPTY_STATE: StorageState = { cookies: [], origins: [] };

function keyFor(tenantId: string): string {
  return `auth:${tenantId}:browser_state`;
}

// ----------------------------------------------------------------------------
// Encryption
// ----------------------------------------------------------------------------

function hexToBytes(hex: string): Uint8Array {
  const clean = hex.trim().replace(/\s+/g, "");
  if (!/^[0-9a-fA-F]+$/.test(clean) || clean.length % 2 !== 0) {
    throw new Error("AUTH_STATE_ENCRYPTION_KEY must be hex of even length");
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < clean.length; i += 2) {
    out[i / 2] = parseInt(clean.slice(i, i + 2), 16);
  }
  return out;
}

function bytesToBase64(bytes: Uint8Array): string {
  // Avoid importing node:Buffer — use a runtime-agnostic path.
  let binary = "";
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return (globalThis as any).btoa(binary);
}

function base64ToBytes(b64: string): Uint8Array {
  const binary = (globalThis as any).atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

let cachedKey: Promise<CryptoKey> | null = null;
async function getEncryptionKey(): Promise<CryptoKey> {
  if (cachedKey) return cachedKey;
  cachedKey = (async () => {
    const raw = env("AUTH_STATE_ENCRYPTION_KEY") ?? "";
    if (!raw) {
      throw new Error(
        "AUTH_STATE_ENCRYPTION_KEY is not set. Generate with: openssl rand -hex 32"
      );
    }
    const bytes = hexToBytes(raw);
    if (bytes.length !== 32) {
      throw new Error(
        `AUTH_STATE_ENCRYPTION_KEY must be exactly 32 bytes (64 hex chars), got ${bytes.length}`
      );
    }
    const subtle = (globalThis as any).crypto?.subtle;
    if (!subtle) throw new Error("Web Crypto subtle not available in this runtime");
    return subtle.importKey(
      "raw",
      bytes,
      { name: "AES-GCM" },
      false,
      ["encrypt", "decrypt"]
    );
  })();
  return cachedKey;
}

async function encryptJson(value: unknown): Promise<string> {
  const subtle = (globalThis as any).crypto?.subtle;
  const key = await getEncryptionKey();
  const iv = (globalThis as any).crypto.getRandomValues(new Uint8Array(12));
  const plaintext = new TextEncoder().encode(JSON.stringify(value));
  const cipher = new Uint8Array(
    await subtle.encrypt({ name: "AES-GCM", iv }, key, plaintext)
  );
  const combined = new Uint8Array(iv.length + cipher.length);
  combined.set(iv, 0);
  combined.set(cipher, iv.length);
  return "v1:" + bytesToBase64(combined);
}

async function decryptJson<T>(blob: string): Promise<T | null> {
  if (!blob || typeof blob !== "string") return null;
  if (!blob.startsWith("v1:")) return null;
  const subtle = (globalThis as any).crypto?.subtle;
  const key = await getEncryptionKey();
  const combined = base64ToBytes(blob.slice(3));
  if (combined.length < 13) return null;
  const iv = combined.subarray(0, 12);
  const cipher = combined.subarray(12);
  try {
    const plain = await subtle.decrypt({ name: "AES-GCM", iv }, key, cipher);
    return JSON.parse(new TextDecoder().decode(plain)) as T;
  } catch {
    return null;
  }
}

// ----------------------------------------------------------------------------
// Public API
// ----------------------------------------------------------------------------

export async function loadStorageState(tenantId: string): Promise<StorageState> {
  if (!env("AUTH_STATE_ENCRYPTION_KEY")) return { ...EMPTY_STATE };
  const store = getStore();
  const blob = await store.get<string>(keyFor(tenantId));
  if (!blob) return { ...EMPTY_STATE };
  const decoded = await decryptJson<StorageState>(blob);
  return decoded ?? { ...EMPTY_STATE };
}

export async function saveStorageState(
  tenantId: string,
  state: StorageState
): Promise<void> {
  if (!env("AUTH_STATE_ENCRYPTION_KEY")) {
    throw new Error("AUTH_STATE_ENCRYPTION_KEY not set — refusing to store auth state in plaintext");
  }
  const store = getStore();
  const blob = await encryptJson(state);
  await store.set(keyFor(tenantId), blob);
}

// Inspect WHICH hostnames the tenant has cookies for. Doesn't expose values.
export async function listCookieDomains(tenantId: string): Promise<string[]> {
  const state = await loadStorageState(tenantId);
  const set = new Set<string>();
  for (const c of state.cookies ?? []) {
    if (c.domain) set.add(c.domain.replace(/^\./, ""));
  }
  for (const o of state.origins ?? []) {
    try {
      set.add(new URL(o.origin).host);
    } catch {
      // skip
    }
  }
  return Array.from(set).sort();
}

// Remove all cookies + localStorage entries for one hostname (and subdomains).
// Returns the count of cookies removed, for user feedback.
export async function forgetHostname(
  tenantId: string,
  hostname: string
): Promise<number> {
  const state = await loadStorageState(tenantId);
  const target = hostname.replace(/^\./, "").toLowerCase();

  const beforeCookies = state.cookies?.length ?? 0;
  state.cookies = (state.cookies ?? []).filter((c) => {
    const d = c.domain.replace(/^\./, "").toLowerCase();
    // Drop exact-match, parent-domain-match, and subdomain-match.
    return !(d === target || d.endsWith("." + target) || target.endsWith("." + d));
  });
  state.origins = (state.origins ?? []).filter((o) => {
    try {
      const h = new URL(o.origin).host.toLowerCase();
      return !(h === target || h.endsWith("." + target));
    } catch {
      return true;
    }
  });
  const removed = beforeCookies - (state.cookies?.length ?? 0);
  await saveStorageState(tenantId, state);
  return removed;
}

export async function forgetAll(tenantId: string): Promise<void> {
  const store = getStore();
  await store.del(keyFor(tenantId));
}
