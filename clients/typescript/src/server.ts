import { LightspeedRpcError, LightspeedTransportError } from "./errors.js";
import type { LightspeedClient } from "./client.js";
import type { Method } from "./generated/methods.js";

const DEFAULT_MAX_BODY_BYTES = 1024 * 1024;

export interface RpcProxyCall {
  /** The incoming request, for reading the caller's login. */
  request: Request;
  method: Method;
  /** Exactly what the browser sent, not yet validated by anything. */
  params: unknown;
}

export type RpcProxyDecision =
  | { deny: "unauthenticated" | "forbidden"; message?: string }
  | {
      deny?: never;
      /** Attributes the call to this user. The client's key must be allowed to assert it. */
      actor?: string;
      /** Replaces the browser's params, for example to force a filter. */
      params?: unknown;
    };

export interface RpcProxyOptions {
  /** Holds the API key, which never reaches the browser. */
  client: LightspeedClient;
  /** The only methods this proxy forwards. */
  methods: readonly Method[];
  /** Decides each call. Called for every forwarded call, after the method check. */
  authorize: (call: RpcProxyCall) => RpcProxyDecision | Promise<RpcProxyDecision>;
  /** Largest request body accepted, 1 MiB by default. Raise it to allow `blobs/put`. */
  maxBodyBytes?: number;
}

/**
 * A request handler that lets a browser use `LightspeedClient` without holding
 * an API key. Point the browser's client at wherever this is mounted.
 *
 * Nothing identifying the browser is forwarded: the key, universe and actor come
 * only from `options`. Only POST with a JSON body is accepted, so a cross-site
 * form cannot use a logged-in user's cookies, and no CORS headers are sent. If
 * `authorize` throws, the returned promise rejects and nothing is forwarded.
 */
export function createRpcProxy(options: RpcProxyOptions): (request: Request) => Promise<Response> {
  const allowed = new Set<string>(options.methods);
  const maxBodyBytes = options.maxBodyBytes ?? DEFAULT_MAX_BODY_BYTES;

  return async (request) => {
    if (request.method !== "POST") {
      return refusal(405, "POST required", { allow: "POST" });
    }
    const contentType = request.headers.get("content-type") ?? "";
    if (contentType.split(";")[0]?.trim().toLowerCase() !== "application/json") {
      return refusal(415, "application/json required");
    }
    const text = await readBody(request, maxBodyBytes);
    if (text === null) {
      return refusal(413, "request body too large");
    }

    let body: unknown;
    try {
      body = JSON.parse(text);
    } catch {
      return rpcError(undefined, -32602, "invalidRequest", "request body is not valid JSON");
    }
    if (typeof body !== "object" || body === null || !("method" in body)) {
      return rpcError(undefined, -32602, "invalidRequest", "request has no method");
    }
    const { id: rawId, method, params } = body as { id?: unknown; method: unknown; params?: unknown };
    const id = typeof rawId === "number" || typeof rawId === "string" ? rawId : undefined;
    if (typeof method !== "string" || !allowed.has(method)) {
      return rpcError(id, -32003, "forbidden", "method is not available here");
    }

    const decision = await options.authorize({ request, method: method as Method, params });
    if (decision.deny !== undefined) {
      const code = decision.deny === "unauthenticated" ? -32001 : -32003;
      const message =
        decision.message ?? (decision.deny === "unauthenticated" ? "sign in required" : "not allowed");
      return rpcError(id, code, decision.deny, message);
    }

    const client =
      decision.actor === undefined ? options.client : options.client.withActor(decision.actor);
    const forwarded = "params" in decision ? decision.params : params;
    try {
      const result = await client.call(method as Method, (forwarded ?? {}) as never, {
        signal: request.signal,
      });
      return json({ ...(id === undefined ? {} : { id }), result });
    } catch (error) {
      if (error instanceof LightspeedRpcError) {
        return json({ ...(id === undefined ? {} : { id }), error: error.payload });
      }
      if (error instanceof LightspeedTransportError) {
        // Its message can name the gateway's internal address.
        return refusal(502, "Lightspeed gateway is unavailable");
      }
      if (request.signal.aborted) {
        return refusal(499, "request cancelled");
      }
      throw error;
    }
  };
}

function rpcError(
  id: number | string | undefined,
  code: number,
  kind: "invalidRequest" | "unauthenticated" | "forbidden",
  message: string,
): Response {
  return json({
    ...(id === undefined ? {} : { id }),
    error: { code, message, data: { kind, message } },
  });
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });
}

function refusal(status: number, error: string, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify({ error }), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store", ...headers },
  });
}

/** The body as text, or null past the limit, however the body is framed. */
async function readBody(request: Request, maxBytes: number): Promise<string | null> {
  if (Number(request.headers.get("content-length")) > maxBytes) {
    return null;
  }
  if (!request.body) {
    return "";
  }
  const reader = request.body.getReader();
  const decoder = new TextDecoder();
  let text = "";
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) {
      return text + decoder.decode();
    }
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      return null;
    }
    text += decoder.decode(value, { stream: true });
  }
}
