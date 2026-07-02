import type {
  AgentApiOutcomeOfRunStartResponse,
  AgentApiOutcomeOfSessionEventsReadResponse,
  EventCursor,
  InputItem,
  RunStartConfig,
  RunStartParams,
  RunStartSource,
  SessionEventView,
  SessionEventsReadParams,
} from "./generated/types.js";
import type {
  Method,
  MethodParams,
  MethodResult,
  RpcCaller,
} from "./generated/methods.js";
import { LightspeedRpcError, LightspeedTransportError, type JsonRpcErrorPayload } from "./errors.js";

export type RequestId = number | string;

export interface CallOptions {
  headers?: HeadersInit;
  signal?: AbortSignal;
}

export interface LightspeedClientOptions {
  endpoint: string | URL;
  fetch?: typeof fetch;
  headers?: HeadersInit | (() => HeadersInit | Promise<HeadersInit>);
  requestId?: () => RequestId;
}

interface JsonRpcResponse {
  id?: RequestId;
  result?: unknown;
  error?: JsonRpcErrorPayload;
}

export interface StartRunOptions extends CallOptions {
  config?: RunStartConfig | null;
  submissionId?: string | null;
}

export interface ReadEventsOptions extends CallOptions {
  after?: EventCursor | null;
  limit?: number | null;
  waitMs?: number | null;
}

export type RunTerminalState =
  | {
      status: "completed";
      event: SessionEventView;
      outputRef: string | null;
    }
  | {
      status: "failed";
      event: SessionEventView;
      message: string;
    }
  | {
      status: "cancelled";
      event: SessionEventView;
    };

export interface AwaitRunOptions extends CallOptions {
  after?: EventCursor | null;
  limit?: number | null;
  waitMs?: number | null;
  heartbeat?: (
    cursor: EventCursor | null,
    page: AgentApiOutcomeOfSessionEventsReadResponse,
  ) => void | Promise<void>;
  onEvent?: (event: SessionEventView) => void | Promise<void>;
  onPage?: (page: AgentApiOutcomeOfSessionEventsReadResponse) => void | Promise<void>;
}

export interface AwaitRunResult {
  state: RunTerminalState;
  cursor: EventCursor;
  page: AgentApiOutcomeOfSessionEventsReadResponse;
}

export class LightspeedClient implements RpcCaller {
  private readonly endpoint: string;
  private readonly fetchImpl: typeof fetch;
  private readonly headers: LightspeedClientOptions["headers"];
  private readonly requestId: (() => RequestId) | undefined;
  private nextNumericRequestId = 1;

  constructor(endpoint: string | URL);
  constructor(options: LightspeedClientOptions);
  constructor(options: string | URL | LightspeedClientOptions) {
    const resolved =
      typeof options === "string" || options instanceof URL ? { endpoint: options } : options;
    const fetchImpl = resolved.fetch ?? globalThis.fetch;
    if (!fetchImpl) {
      throw new LightspeedTransportError("LightspeedClient requires a fetch implementation");
    }
    this.endpoint = resolved.endpoint.toString();
    this.fetchImpl = fetchImpl;
    this.headers = resolved.headers;
    this.requestId = resolved.requestId;
  }

  async call<M extends Method>(
    method: M,
    params: MethodParams<M>,
    options: CallOptions = {},
  ): Promise<MethodResult<M>> {
    const id = this.nextRequestId();
    const headers = await this.buildHeaders(options.headers);
    const init: RequestInit = {
      method: "POST",
      headers,
      body: JSON.stringify({ id, method, params }),
    };
    if (options.signal) {
      init.signal = options.signal;
    }

    let response: Response;
    try {
      response = await this.fetchImpl(this.endpoint, init);
    } catch (error) {
      throw new LightspeedTransportError(`Lightspeed API request failed: ${errorMessage(error)}`);
    }

    const body = await readJson(response);
    if (!response.ok) {
      throw new LightspeedTransportError(`Lightspeed API request failed with HTTP ${response.status}`, {
        status: response.status,
        body,
      });
    }
    if (!isRecord(body)) {
      throw new LightspeedTransportError("Lightspeed API response was not a JSON object", { body });
    }

    const rpcResponse = body as JsonRpcResponse;
    if (rpcResponse.id !== undefined && rpcResponse.id !== id) {
      throw new LightspeedTransportError("Lightspeed API response id did not match the request id", {
        body,
      });
    }
    if (rpcResponse.error) {
      throw new LightspeedRpcError(rpcResponse.error);
    }
    if (!("result" in rpcResponse)) {
      throw new LightspeedTransportError("Lightspeed API response was missing result", { body });
    }
    return rpcResponse.result as MethodResult<M>;
  }

  startRun(
    sessionId: string,
    input: InputItem[],
    options: StartRunOptions = {},
  ): Promise<AgentApiOutcomeOfRunStartResponse> {
    return this.startRunWithSource(sessionId, { type: "input", items: input }, options);
  }

  startRunFromContext(
    sessionId: string,
    keys: string[],
    options: StartRunOptions = {},
  ): Promise<AgentApiOutcomeOfRunStartResponse> {
    return this.startRunWithSource(sessionId, { type: "context", keys }, options);
  }

  startRunWithSource(
    sessionId: string,
    source: RunStartSource,
    options: StartRunOptions = {},
  ): Promise<AgentApiOutcomeOfRunStartResponse> {
    const params: RunStartParams = {
      sessionId,
      source,
      submissionId:
        options.submissionId === undefined ? generateSubmissionId() : options.submissionId,
    };
    if ("config" in options) {
      params.config = options.config ?? null;
    }
    return this.call("run/start", params, options);
  }

  readEvents(
    sessionId: string,
    options: ReadEventsOptions = {},
  ): Promise<AgentApiOutcomeOfSessionEventsReadResponse> {
    const params: SessionEventsReadParams = { sessionId };
    if (options.after !== undefined) {
      params.after = options.after;
    }
    if (options.limit !== undefined) {
      params.limit = options.limit;
    }
    if (options.waitMs !== undefined) {
      params.waitMs = options.waitMs;
    }
    return this.call("session/events/read", params, options);
  }

  async awaitRun(
    sessionId: string,
    runId: string,
    options: AwaitRunOptions = {},
  ): Promise<AwaitRunResult> {
    let cursor = options.after ?? null;
    for (;;) {
      throwIfAborted(options.signal);
      const readOptions: ReadEventsOptions = {
        after: cursor,
        waitMs: options.waitMs ?? 30_000,
      };
      if (options.limit !== undefined) {
        readOptions.limit = options.limit;
      }
      if (options.signal) {
        readOptions.signal = options.signal;
      }
      if (options.headers) {
        readOptions.headers = options.headers;
      }
      const page = await this.readEvents(sessionId, readOptions);
      await options.onPage?.(page);

      for (const event of page.result.events ?? []) {
        cursor = event.cursor;
        await options.onEvent?.(event);
        const state = terminalStateForRun(event, runId);
        if (state) {
          await options.heartbeat?.(cursor, page);
          return { state, cursor, page };
        }
      }

      cursor = page.result.nextCursor ?? page.result.headCursor ?? cursor;
      await options.heartbeat?.(cursor, page);
    }
  }

  private nextRequestId(): RequestId {
    if (this.requestId) {
      return this.requestId();
    }
    return this.nextNumericRequestId++;
  }

  private async buildHeaders(callHeaders: HeadersInit | undefined): Promise<Headers> {
    const headers = new Headers();
    headers.set("content-type", "application/json");
    headers.set("accept", "application/json");
    const defaultHeaders =
      typeof this.headers === "function" ? await this.headers() : this.headers;
    appendHeaders(headers, defaultHeaders);
    appendHeaders(headers, callHeaders);
    return headers;
  }
}

export function generateSubmissionId(): string {
  if (globalThis.crypto?.randomUUID) {
    return globalThis.crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto?.getRandomValues(bytes);
  if (bytes.some((byte) => byte !== 0)) {
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  return `sub_${Date.now().toString(36)}_${Math.random().toString(36).slice(2)}`;
}

function terminalStateForRun(event: SessionEventView, runId: string): RunTerminalState | null {
  const kind = event.kind;
  switch (kind.type) {
    case "runCompleted":
      return kind.runId === runId
        ? { status: "completed", event, outputRef: kind.outputRef ?? null }
        : null;
    case "runFailed":
      return kind.runId === runId ? { status: "failed", event, message: kind.message } : null;
    case "runCancelled":
      return kind.runId === runId ? { status: "cancelled", event } : null;
    default:
      return null;
  }
}

function appendHeaders(headers: Headers, values: HeadersInit | undefined): void {
  if (!values) {
    return;
  }
  new Headers(values).forEach((value, key) => headers.set(key, value));
}

async function readJson(response: Response): Promise<unknown> {
  const text = await response.text();
  if (text.length === 0) {
    return null;
  }
  try {
    return JSON.parse(text) as unknown;
  } catch (error) {
    throw new LightspeedTransportError(`Lightspeed API response was not valid JSON: ${errorMessage(error)}`, {
      status: response.status,
      body: text,
    });
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted) {
    throw signal.reason instanceof Error ? signal.reason : new Error("operation aborted");
  }
}
