import {
  LightspeedClient,
  LightspeedTransportError,
  type LightspeedClientOptions,
} from "@lightspeed/agent-client";
import type { RequestAuthContext } from "./request-auth.js";
import { upstreamHeaders } from "./request-auth.js";

export interface UpstreamClientFactoryOptions {
  endpoint: string | URL;
  fetch?: typeof fetch;
  maxResponseBytes?: number;
}

export type UpstreamClientFactory = (auth: RequestAuthContext) => LightspeedClient;

export function createUpstreamClientFactory(
  options: UpstreamClientFactoryOptions,
): UpstreamClientFactory {
  const baseFetch = options.fetch ?? globalThis.fetch;
  const fetchImpl =
    options.maxResponseBytes === undefined
      ? baseFetch
      : responseLimitedFetch(baseFetch, options.maxResponseBytes);
  return (auth) => {
    const clientOptions: LightspeedClientOptions = {
      endpoint: options.endpoint,
    };
    const headers = upstreamHeaders(auth);
    if (headers !== undefined) {
      clientOptions.headers = headers;
    }
    clientOptions.fetch = fetchImpl;
    return new LightspeedClient(clientOptions);
  };
}

export async function validateUpstreamIdentity(
  factory: UpstreamClientFactory,
  auth: RequestAuthContext,
  signal: AbortSignal,
  timeoutMs: number,
): Promise<void> {
  await factory(auth).call(
    "initialize",
    {
      clientInfo: { name: "lightspeed-configurator-mcp", version: "0.0.0" },
      capabilities: null,
    },
    { signal: requestSignal(signal, timeoutMs) },
  );
}

export function requestSignal(signal: AbortSignal, timeoutMs: number): AbortSignal {
  return AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)]);
}

function responseLimitedFetch(baseFetch: typeof fetch, maxBytes: number): typeof fetch {
  return async (input, init) => {
    const response = await baseFetch(input, init);
    const declaredLength = Number(response.headers.get("content-length"));
    if (Number.isFinite(declaredLength) && declaredLength > maxBytes) {
      await response.body?.cancel();
      throw new LightspeedTransportError(
        `Lightspeed API response exceeded ${maxBytes} bytes`,
        { status: response.status },
      );
    }
    if (!response.body) {
      return response;
    }
    const reader = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let total = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      total += value.byteLength;
      if (total > maxBytes) {
        await reader.cancel();
        throw new LightspeedTransportError(
          `Lightspeed API response exceeded ${maxBytes} bytes`,
          { status: response.status },
        );
      }
      chunks.push(value);
    }
    const body = new Uint8Array(total);
    let offset = 0;
    for (const chunk of chunks) {
      body.set(chunk, offset);
      offset += chunk.byteLength;
    }
    return new Response(body, {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  };
}
