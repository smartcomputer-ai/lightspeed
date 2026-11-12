import type {
  EventCursor,
  InputItem,
  RunStartResponse,
  RunSteerParams,
  RunSteerResponse,
  RunStartConfig,
  RunStartParams,
  RunStatus,
  RunView,
  SessionEventKindView,
  SessionEventView,
  SessionEventsReadParams,
  SessionEventsReadResponse,
} from "./generated/types.js";
import type {
  Method,
  MethodParams,
  MethodResult,
} from "./generated/methods.js";
import { LightspeedHelpers } from "./generated/helpers.js";
import { METHOD_INFO } from "./generated/methods.js";
import { LightspeedRpcError, LightspeedTransportError, type JsonRpcErrorPayload } from "./errors.js";

export type RequestId = number | string;

export interface CallOptions {
  headers?: HeadersInit;
  signal?: AbortSignal;
}

export interface LightspeedClientOptions {
  /** The gateway's `/rpc` URL. */
  endpoint: string | URL;
  /** Sent as the bearer credential. Omit it only for a gateway in single mode. */
  apiKey?: string;
  /**
   * The universe a deployment key addresses, as its UUID or its handle. A
   * universe key must leave this out.
   */
  universe?: string;
  /**
   * Attributes requests to this opaque ID, which the key must be allowed to
   * assert. It does not limit what the key can do. See `withActor`.
   */
  actor?: string;
  fetch?: typeof fetch;
  headers?: HeadersInit | (() => HeadersInit | Promise<HeadersInit>);
  requestId?: () => RequestId;
}

/** A method's data, without the `AgentApiOutcome` envelope around it. */
export type MethodData<M extends Method> = MethodResult<M>["result"];

export type SessionEventType = SessionEventKindView["type"];

/** One kind of session event, such as `SessionEventKindOf<"runFailed">`. */
export type SessionEventKindOf<T extends SessionEventType> = Extract<
  SessionEventKindView,
  { type: T }
>;

interface JsonRpcResponse {
  id?: RequestId;
  result?: unknown;
  error?: JsonRpcErrorPayload;
}

export interface StartRunOptions extends CallOptions {
  config?: RunStartConfig | null;
  submissionId?: string | null;
}

export interface SteerRunOptions extends CallOptions {
  submissionId?: string | null;
}

export interface ReadEventsOptions extends CallOptions {
  after?: EventCursor | null;
  limit?: number | null;
  waitMs?: number | null;
}

export type TerminalRunStatus = Extract<RunStatus, "completed" | "failed" | "cancelled">;

export type TerminalRunView = RunView & { status: TerminalRunStatus };

export interface AwaitRunOptions extends CallOptions {
  after?: EventCursor | null;
  limit?: number | null;
  waitMs?: number | null;
  heartbeat?: (
    cursor: EventCursor | null,
    page: SessionEventsReadResponse,
  ) => void | Promise<void>;
  /** Called for this run's own events. `onPage` sees every event in the session. */
  onEvent?: (event: SessionEventView) => void | Promise<void>;
  onPage?: (page: SessionEventsReadResponse) => void | Promise<void>;
}

export interface AwaitRunResult {
  /** The finished run, as `session/runs/read` projects it. */
  run: TerminalRunView;
  /** Where to resume reading the session's events. Null if none were read. */
  cursor: EventCursor | null;
}

const AWAIT_RUN_WAIT_MS = 30_000;
const AWAIT_RUN_MIN_WAIT_MS = 1_000;
/** Bounds a request beyond its long-poll, so a stalled connection cannot hold a wait open. */
const AWAIT_RUN_REQUEST_GRACE_MS = 30_000;
/** Six attempts span about 30 seconds of backoff, enough to outlast a runtime restart. */
const AWAIT_RUN_READ_ATTEMPTS = 6;
const AWAIT_RUN_RETRY_BASE_MS = 1_000;

/**
 * A client for the Lightspeed API. `call` reaches every method and returns its
 * full envelope. Named helpers cover what most applications need, return just the
 * method's data, and take the IDs of their target as positional arguments.
 */
export class LightspeedClient extends LightspeedHelpers {
  private readonly options: LightspeedClientOptions;
  private readonly endpoint: string;
  private readonly fetchImpl: typeof fetch;
  private readonly headers: LightspeedClientOptions["headers"];
  private readonly requestId: (() => RequestId) | undefined;
  private nextNumericRequestId = 1;

  constructor(endpoint: string | URL);
  constructor(options: LightspeedClientOptions);
  constructor(options: string | URL | LightspeedClientOptions) {
    super();
    const resolved =
      typeof options === "string" || options instanceof URL ? { endpoint: options } : options;
    const fetchImpl = resolved.fetch ?? globalThis.fetch;
    if (!fetchImpl) {
      throw new LightspeedTransportError("LightspeedClient requires a fetch implementation");
    }
    this.options = resolved;
    this.endpoint = resolved.endpoint.toString();
    this.fetchImpl = fetchImpl;
    this.headers = resolved.headers;
    this.requestId = resolved.requestId;
  }

  /** A client like this one whose requests are attributed to `actor`. */
  withActor(actor: string): LightspeedClient {
    return new LightspeedClient({ ...this.options, actor });
  }

  async call<M extends Method>(
    method: M,
    params: MethodParams<M>,
    options: CallOptions = {},
  ): Promise<MethodResult<M>> {
    const id = this.nextRequestId();
    const headers = await this.buildHeaders(options.headers);
    if (METHOD_INFO[method].scope === "connection" || METHOD_INFO[method].scope === "deployment") {
      headers.delete("x-lightspeed-universe");
    }
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
      // Called detached: browsers reject a fetch invoked as a method of another object.
      const { fetchImpl } = this;
      response = await fetchImpl(this.endpoint, init);
    } catch (error) {
      options.signal?.throwIfAborted();
      throw new LightspeedTransportError(`Lightspeed API request failed: ${errorMessage(error)}`, {
        cause: error,
      });
    }

    const body = await this.readBody(response, options.signal);
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

  /**
   * Starts a run. A submission ID is generated if you give none, but only one you
   * keep lets a retry find the same run after a lost response.
   */
  async startRun(
    sessionId: string,
    input: string | InputItem[],
    options: StartRunOptions = {},
  ): Promise<RunStartResponse> {
    const params: RunStartParams = {
      sessionId,
      source: { type: "input", items: inputItems(input) },
      submissionId:
        options.submissionId === undefined ? generateSubmissionId() : options.submissionId,
    };
    if ("config" in options) {
      params.config = options.config ?? null;
    }
    const { result } = await this.call("session/runs/start", params, callOptions(options));
    return result;
  }

  /** Sends input to a run in progress, which its next model turn receives. */
  async steerRun(
    sessionId: string,
    runId: string,
    input: string | InputItem[],
    options: SteerRunOptions = {},
  ): Promise<RunSteerResponse> {
    const params: RunSteerParams = {
      sessionId,
      runId,
      items: inputItems(input),
      submissionId:
        options.submissionId === undefined ? generateSubmissionId() : options.submissionId,
    };
    const { result } = await this.call("session/runs/steer", params, callOptions(options));
    return result;
  }

  async readEvents(
    sessionId: string,
    options: ReadEventsOptions = {},
  ): Promise<SessionEventsReadResponse> {
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
    const { result } = await this.call("session/events/read", params, callOptions(options));
    return result;
  }

  /**
   * Waits until a run finishes and returns it. Repeats reads that fail on the
   * network, and rejects with `sessionExecutionStopped` when nothing can finish
   * the run. Reads the session from its beginning unless given `after`. Giving
   * up on the wait does not cancel the run.
   */
  async awaitRun(
    sessionId: string,
    runId: string,
    options: AwaitRunOptions = {},
  ): Promise<AwaitRunResult> {
    const waitMs = Math.max(options.waitMs ?? AWAIT_RUN_WAIT_MS, AWAIT_RUN_MIN_WAIT_MS);
    const read = <T>(waited: number, request: (call: CallOptions) => Promise<T>) =>
      retryTransient(waited + AWAIT_RUN_REQUEST_GRACE_MS, options, request);
    const readRun = async (): Promise<RunView> => {
      const { result } = await read(0, (call) =>
        this.call("session/runs/read", { sessionId, runId }, call),
      );
      return result.run;
    };

    let cursor = options.after ?? null;
    // The first read does not wait, so a run that already ended, or a session
    // that has stopped, is reported at once.
    let currentWaitMs = 0;
    for (;;) {
      const readOptions: ReadEventsOptions = { after: cursor, waitMs: currentWaitMs };
      if (options.limit !== undefined) {
        readOptions.limit = options.limit;
      }
      const page = await read(currentWaitMs, (call) =>
        this.readEvents(sessionId, { ...readOptions, ...call }),
      );
      await options.onPage?.(page);

      const events = page.events ?? [];
      let finished = false;
      for (const event of events) {
        cursor = event.cursor;
        if (event.joins.runId === runId) {
          await options.onEvent?.(event);
        }
        if (isTerminalEventFor(event, runId)) {
          finished = true;
          break;
        }
      }
      if (!finished) {
        // Never advance to `headCursor`: it is read after the page, so an event
        // landing between the two would be skipped.
        cursor = page.nextCursor ?? cursor;
      }
      await options.heartbeat?.(cursor, page);
      if (finished) {
        return { run: expectTerminal(await readRun()), cursor };
      }

      if (events.length === 0) {
        // An empty page says nothing about the run, and the terminal event may
        // be behind a caller's cursor, so ask for its state.
        const { result } = await read(0, (call) =>
          this.call("session/read", { sessionId }, call),
        );
        const summary = result.session.runs?.find((run) => run.id === runId);
        if (!summary || isTerminalStatus(summary.status)) {
          const run = await readRun();
          if (isTerminalRun(run)) {
            return { run, cursor };
          }
        }
        if (result.session.executionFailure) {
          const message = `session ${sessionId} execution has stopped (${result.session.executionFailure})`;
          throw new LightspeedRpcError({
            code: -32016,
            message,
            data: { kind: "sessionExecutionStopped", message },
          });
        }
      }
      currentWaitMs = waitMs;
    }
  }

  /** A blob's bytes. Refs come from run output, tool calls, and message entries. */
  async readBlobBytes(blobRef: string, options: CallOptions = {}): Promise<Uint8Array> {
    const { result } = await this.call("blobs/read", { blobRef }, options);
    return Uint8Array.from(atob(result.bytesBase64), (char) => char.charCodeAt(0));
  }

  /**
   * A blob as text, rejecting with a `TypeError` if it is not valid UTF-8. Text
   * is not always what a blob holds: model output can be provider JSON or media.
   */
  async readBlobText(blobRef: string, options: CallOptions = {}): Promise<string> {
    return new TextDecoder("utf-8", { fatal: true }).decode(await this.readBlobBytes(blobRef, options));
  }

  /** Stores bytes, or text as UTF-8, and returns the reference to use them by. */
  async putBlob(data: string | Uint8Array, options: CallOptions = {}): Promise<string> {
    const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
    let binary = "";
    for (const byte of bytes) {
      binary += String.fromCharCode(byte);
    }
    const { result } = await this.call(
      "blobs/put",
      { blobs: [{ bytesBase64: btoa(binary) }] },
      options,
    );
    const stored = result.blobs?.[0];
    if (!stored) {
      throw new LightspeedTransportError("Lightspeed API stored no blob");
    }
    return stored.blobRef;
  }

  private nextRequestId(): RequestId {
    if (this.requestId) {
      return this.requestId();
    }
    return this.nextNumericRequestId++;
  }

  private async readBody(response: Response, signal: AbortSignal | undefined): Promise<unknown> {
    let text: string;
    try {
      text = await response.text();
    } catch (error) {
      signal?.throwIfAborted();
      throw new LightspeedTransportError(`Lightspeed API response failed: ${errorMessage(error)}`, {
        cause: error,
      });
    }
    let body: unknown = null;
    let parseFailure: unknown;
    if (text.length > 0) {
      try {
        body = JSON.parse(text) as unknown;
      } catch (error) {
        parseFailure = error;
        body = text;
      }
    }
    if (!response.ok) {
      // A proxy's error page is not JSON, and the status is what explains it.
      const hint =
        response.status === 404 ? `: check that ${this.endpoint} is the gateway's /rpc URL` : "";
      throw new LightspeedTransportError(
        `Lightspeed API request failed with HTTP ${response.status}${hint}`,
        { status: response.status, body },
      );
    }
    if (parseFailure !== undefined) {
      throw new LightspeedTransportError(
        `Lightspeed API response was not valid JSON: ${errorMessage(parseFailure)}`,
        { status: response.status, body, cause: parseFailure },
      );
    }
    return body;
  }

  private async buildHeaders(callHeaders: HeadersInit | undefined): Promise<Headers> {
    const headers = new Headers();
    headers.set("content-type", "application/json");
    headers.set("accept", "application/json");
    if (this.options.apiKey !== undefined) {
      headers.set("authorization", `Bearer ${this.options.apiKey}`);
    }
    if (this.options.universe !== undefined) {
      headers.set("x-lightspeed-universe", this.options.universe);
    }
    if (this.options.actor !== undefined) {
      headers.set("x-lightspeed-actor", this.options.actor);
    }
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

function inputItems(input: string | InputItem[]): InputItem[] {
  return typeof input === "string" ? [{ type: "text", text: input }] : input;
}

/** Just the transport options, from an options object that also holds params. */
function callOptions(options: CallOptions): CallOptions {
  const call: CallOptions = {};
  if (options.signal) {
    call.signal = options.signal;
  }
  if (options.headers) {
    call.headers = options.headers;
  }
  return call;
}

function isTerminalEventFor(event: SessionEventView, runId: string): boolean {
  const kind = event.kind;
  return (
    (kind.type === "runCompleted" || kind.type === "runFailed" || kind.type === "runCancelled") &&
    kind.runId === runId
  );
}

function isTerminalStatus(status: RunStatus): status is TerminalRunStatus {
  return status === "completed" || status === "failed" || status === "cancelled";
}

function isTerminalRun(run: RunView): run is TerminalRunView {
  return isTerminalStatus(run.status);
}

function expectTerminal(run: RunView): TerminalRunView {
  if (!isTerminalRun(run)) {
    throw new LightspeedTransportError(
      `Lightspeed API reported run ${run.id} as ${run.status} after its terminal event`,
    );
  }
  return run;
}

/** Repeats a read, which is always safe, through failures that may pass. */
async function retryTransient<T>(
  timeoutMs: number,
  options: CallOptions,
  request: (call: CallOptions) => Promise<T>,
): Promise<T> {
  const { signal } = options;
  for (let attempt = 1; ; attempt += 1) {
    signal?.throwIfAborted();
    const timeout = AbortSignal.timeout(timeoutMs);
    const call: CallOptions = { signal: signal ? AbortSignal.any([signal, timeout]) : timeout };
    if (options.headers) {
      call.headers = options.headers;
    }
    try {
      return await request(call);
    } catch (error) {
      signal?.throwIfAborted();
      const retryable =
        timeout.aborted || (error instanceof LightspeedTransportError && error.transient);
      if (!retryable || attempt >= AWAIT_RUN_READ_ATTEMPTS) {
        throw error;
      }
      await sleep(AWAIT_RUN_RETRY_BASE_MS * 2 ** (attempt - 1), signal);
    }
  }
}

function sleep(ms: number, signal: AbortSignal | undefined): Promise<void> {
  return new Promise((resolve, reject) => {
    const done = () => {
      signal?.removeEventListener("abort", aborted);
      resolve();
    };
    const aborted = () => {
      clearTimeout(timer);
      reject(signal?.reason);
    };
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", aborted, { once: true });
  });
}

function appendHeaders(headers: Headers, values: HeadersInit | undefined): void {
  if (!values) {
    return;
  }
  new Headers(values).forEach((value, key) => headers.set(key, value));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
