// app/lib/expressBridge.ts
//
// Minimal bridge that lets an Express handler (Node IncomingMessage/Server
// Response based) serve a Next.js App Router request (Web Request → Response).
// Used to mount the upstream @workflow/web Express app under our Next routes
// so the literal upstream dashboard renders at our domain — no iframe, no
// separate sub-app, no port to the upstream code.
//
// Caveats: best-effort. Handles the cases the WDK dashboard actually uses
// (synchronous response writes, JSON/HTML payloads, static asset serving).
// Doesn't try to support streaming responses or websockets.

import { IncomingMessage, ServerResponse } from "node:http";
import { Socket } from "node:net";

type ExpressHandler = (
  req: IncomingMessage,
  res: ServerResponse
) => void | Promise<void>;

export async function expressToFetch(
  app: ExpressHandler,
  req: Request,
  rewriteUrl?: string
): Promise<Response> {
  const url = new URL(req.url);
  const targetUrl = rewriteUrl ?? url.pathname + url.search;

  // Drain body once so we can replay it as a Node-style readable stream.
  let bodyBuf: Buffer | null = null;
  if (
    req.body &&
    req.method !== "GET" &&
    req.method !== "HEAD"
  ) {
    const ab = await req.arrayBuffer();
    bodyBuf = Buffer.from(ab);
  }

  return new Promise<Response>((resolve, reject) => {
    // Fake socket — Express/Node code paths poke at this for remote address etc.
    const socket = new Socket();

    const incoming = new IncomingMessage(socket);
    incoming.method = req.method;
    incoming.url = targetUrl;
    // Headers: lowercased per Node convention.
    const headersObj: Record<string, string> = {};
    req.headers.forEach((v, k) => {
      headersObj[k.toLowerCase()] = v;
    });
    incoming.headers = headersObj;

    // Push body then end.
    process.nextTick(() => {
      if (bodyBuf && bodyBuf.length > 0) {
        incoming.push(bodyBuf);
      }
      incoming.push(null);
    });

    // Collect response.
    const chunks: Buffer[] = [];
    let statusCode = 200;
    const respHeaders: Record<string, string | string[]> = {};
    let resolved = false;

    const res = new ServerResponse(incoming) as ServerResponse & {
      _origWriteHead?: ServerResponse["writeHead"];
    };

    const finalize = () => {
      if (resolved) return;
      resolved = true;
      const body = chunks.length ? Buffer.concat(chunks) : Buffer.alloc(0);
      // Normalize headers to a single string each (Headers accepts arrays via
      // multiple append calls).
      const h = new Headers();
      for (const [name, val] of Object.entries(respHeaders)) {
        if (Array.isArray(val)) {
          for (const v of val) h.append(name, String(v));
        } else if (val !== undefined && val !== null) {
          h.set(name, String(val));
        }
      }
      resolve(new Response(body, { status: statusCode, headers: h }));
    };

    const origWriteHead = res.writeHead.bind(res);
    res.writeHead = ((
      code: number,
      reasonOrHeaders?: string | Record<string, string | string[]> | unknown,
      maybeHeaders?: Record<string, string | string[]>
    ) => {
      statusCode = code;
      const maybe = typeof reasonOrHeaders === "object" && reasonOrHeaders
        ? (reasonOrHeaders as Record<string, string | string[]>)
        : maybeHeaders;
      if (maybe) {
        for (const [k, v] of Object.entries(maybe)) {
          respHeaders[k.toLowerCase()] = v;
        }
      }
      return origWriteHead(code, reasonOrHeaders as never, maybeHeaders as never);
    }) as typeof res.writeHead;

    const origSetHeader = res.setHeader.bind(res);
    res.setHeader = ((name: string, value: number | string | readonly string[]) => {
      const v = Array.isArray(value)
        ? value.map(String)
        : String(value);
      respHeaders[name.toLowerCase()] = v;
      return origSetHeader(name, value as never);
    }) as typeof res.setHeader;

    // Canonical chunk → Buffer conversion. Critical: must preserve binary
    // payloads (CBOR responses from /api/rpc are Uint8Arrays) — going through
    // `String(chunk)` would mangle them and surface as "end of buffer not
    // reached" decode errors in the client.
    const toBuf = (chunk: unknown, encoding?: unknown): Buffer => {
      if (Buffer.isBuffer(chunk)) return chunk as Buffer;
      if (chunk instanceof Uint8Array) return Buffer.from(chunk);
      if (chunk instanceof ArrayBuffer) return Buffer.from(chunk);
      if (typeof chunk === "string") {
        return Buffer.from(
          chunk,
          (encoding as BufferEncoding | undefined) ?? "utf8"
        );
      }
      // Last-resort coercion. Reaching here means upstream wrote something
      // unusual; we still avoid String() to keep binary intact.
      return Buffer.from(chunk as ArrayBufferLike);
    };

    res.write = ((chunk?: unknown, encoding?: unknown, cb?: unknown) => {
      if (chunk != null) chunks.push(toBuf(chunk, encoding));
      if (typeof encoding === "function") {
        (encoding as (e?: Error | null) => void)();
      } else if (typeof cb === "function") {
        (cb as (e?: Error | null) => void)();
      }
      return true;
    }) as typeof res.write;

    res.end = ((chunk?: unknown, encoding?: unknown, cb?: unknown) => {
      if (chunk != null && typeof chunk !== "function") {
        chunks.push(toBuf(chunk, encoding));
      }
      if (typeof chunk === "function") (chunk as () => void)();
      else if (typeof encoding === "function") (encoding as () => void)();
      else if (typeof cb === "function") (cb as () => void)();
      finalize();
      return res;
    }) as typeof res.end;

    Promise.resolve(app(incoming, res)).catch((e) => {
      if (!resolved) reject(e);
    });
  });
}
