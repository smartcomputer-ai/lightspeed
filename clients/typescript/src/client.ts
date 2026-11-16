import { normalizeEndpoint } from "./endpoint.js";
import type { ClientConnectionResponse, ClientOperationId, ClientOperationParams, ClientOperationResult } from "./operations.js";
import { PlatformHelpers } from "./generated/platform-helpers.js";
import { PLATFORM_OPERATIONS, type PlatformOperationId, type PlatformOperationParams } from "./generated/platform.js";
import { readEventStream } from "./event-stream.js";
import type { AgentNotification, UniverseWatchParams } from "./generated/types.js";
import type {
  EventCursor,
  InputItem,
  RunStartResponse,
  RunSteerParams,
  RunSteerResponse,
  RunStartSource,
  RunStartParams,
  RunStatus,
  RunView,
  SessionEventKindView,
  SessionEventView,
  SessionEventsReadParams,
  SessionEventsReadResponse,
} from "./generated/types.js";
import type {
  OperationId,
  OperationParams,
  OperationResult,
} from "./generated/operations.js";
import { LightspeedHelpers } from "./generated/helpers.js";
import { OPERATION_INFO } from "./generated/operations.js";
import { HTTP_OPERATIONS } from "./generated/http.js";
import { prepareHttpRequest } from "./http-request.js";
import type { HttpProblem } from "./generated/types.js";
import { LightspeedApiError, LightspeedTransportError } from "./errors.js";


export interface CallOptions {
  headers?: HeadersInit;
  signal?: AbortSignal;
  /** Raw bytes or stream for binary upload operations. */
  body?: Uint8Array | ReadableStream<Uint8Array>;
  /** Required for stream uploads; byte arrays determine their own length. */
  contentLength?: number;
}

export interface LightspeedClientOptions {
  /** Deployment origin or API base URL; base paths and trailing slashes are supported. */
  endpoint: string | URL;
  /** Bearer credential or per-request provider. Omit for a single runtime,
   * header-based authentication, or same-origin Platform browser session cookies. */
  token?: string | (() => string | undefined | Promise<string | undefined>);
  /** Saved account context; native helpers still take accountId explicitly. */
  accountId?: string;
  /** Persist a rotated CLI session only when this request used the expected credential. */
  onSessionToken?: (token: string, previousToken: string) => void | Promise<void>;
  /**
   * The universe a request addresses, as its UUID or its handle.
   */
  universe?: string;
  /**
   * Attributes requests to this opaque ID. Direct runtime tokens may always
   * name an actor. It does not restrict access. See `withActor`.
   */
  actor?: string;
  fetch?: typeof fetch;
  headers?: HeadersInit | (() => HeadersInit | Promise<HeadersInit>);
}

export type SessionEventType = SessionEventKindView["type"];

/** One kind of session event, such as `SessionEventKindOf<"runFailed">`. */
export type SessionEventKindOf<T extends SessionEventType> = Extract<
  SessionEventKindView,
  { type: T }
>;

export interface WatchOptions extends CallOptions {
  /** Reconnect after stream completion or transient transport errors. Defaults to true. */
  reconnect?: boolean;
}

export type StartRunOptions = Omit<RunStartParams, "sessionId" | "source"> & CallOptions;
export type SteerRunOptions = Omit<RunSteerParams, "sessionId" | "runId" | "items"> & CallOptions;
export type ReadEventsOptions = Omit<SessionEventsReadParams, "sessionId"> & CallOptions;

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

const WATCH_EVENTS: readonly string[] = OPERATION_INFO["universe/watch"].notifications;

const AWAIT_RUN_WAIT_MS = 30_000;
const AWAIT_RUN_MIN_WAIT_MS = 1_000;
/** Bounds a request beyond its long-poll, so a stalled connection cannot hold a wait open. */
const AWAIT_RUN_REQUEST_GRACE_MS = 30_000;
/** Six attempts span about 30 seconds of backoff, enough to outlast a runtime restart. */
const AWAIT_RUN_READ_ATTEMPTS = 6;
const AWAIT_RUN_RETRY_BASE_MS = 1_000;

/**
 * A client for the Lightspeed HTTP API. Calls and named helpers return direct DTOs.
 * Named helpers take the IDs of their target as positional arguments.
 */
export class LightspeedClient extends LightspeedHelpers {
  private readonly options: LightspeedClientOptions;
  private readonly endpoint: string;
  private readonly fetchImpl: typeof fetch;
  private readonly headers: LightspeedClientOptions["headers"];
  readonly accountId: string | undefined;
  readonly platform: PlatformHelpers;

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
    this.endpoint = normalizeEndpoint(resolved.endpoint);
    this.options = { ...resolved, endpoint: this.endpoint };
    this.accountId = resolved.accountId;
    this.platform = new PlatformHelpers(this);
    this.fetchImpl = fetchImpl;
    this.headers = resolved.headers;
  }

  /** Discover runtime or Platform identity with a discriminated typed result. */
  override initialize(options: CallOptions = {}): Promise<ClientConnectionResponse> {
    return this.call("initialize", {}, options);
  }

  /** A client like this one whose requests are attributed to `actor`. */
  withActor(actor: string): LightspeedClient {
    return new LightspeedClient({ ...this.options, actor });
  }

  /** Open one authenticated watch. Useful to proxy its body; cancel it on disconnect. */
  async openWatch(params: UniverseWatchParams = {}, options: CallOptions = {}): Promise<Response> {
    const admission = new AbortController();
    const timer = setTimeout(() => admission.abort(), 15_000);
    let response: Response;
    try {
      response = await this.request("universe/watch", params, {
        ...options,
        signal: options.signal ? AbortSignal.any([options.signal, admission.signal]) : admission.signal,
      });
    } finally { clearTimeout(timer); }
    if (!response.headers.get("content-type")?.startsWith("text/event-stream")) {
      await response.body?.cancel();
      throw new LightspeedTransportError("Watch response was not an event stream");
    }
    if (!response.body) throw new LightspeedTransportError("Watch response has no body");
    return response;
  }

  /** Watch list rows. Refetch on watch/reset and periodically; there is no resume cursor. */
  async *watch(params: UniverseWatchParams = {}, options: WatchOptions = {}): AsyncGenerator<AgentNotification> {
    let attempts = 0;
    while (!options.signal?.aborted) {
      try {
        const response = await this.openWatch(params, options);
        for await (const { event, data } of readEventStream(response.body!)) {
          let params: unknown;
          try { params = JSON.parse(data); } catch { throw new LightspeedTransportError("Invalid watch JSON"); }
          if (!isRecord(params)) throw new LightspeedTransportError("Invalid watch event");
          if (event === "watch/end") break;
          if (!WATCH_EVENTS.includes(event)) {
            throw new LightspeedTransportError("Unknown watch notification", { body: { event, params } });
          }
          attempts = 0;
          yield { method: event, params } as AgentNotification;
        }
      } catch (error) {
        if (options.signal?.aborted) return;
        if (error instanceof LightspeedApiError || options.reconnect === false ||
            (error instanceof LightspeedTransportError && !error.transient)) throw error;
      }
      if (options.reconnect === false) return;
      try { await sleep(Math.min(30_000, 1_000 * 2 ** Math.min(attempts++, 5)), options.signal); }
      catch (error) { if (options.signal?.aborted) return; throw error; }
    }
  }

  async call<M extends ClientOperationId>(
    operation: M,
    params: ClientOperationParams<M>,
    options: CallOptions = {},
  ): Promise<ClientOperationResult<M>> {
    if (Object.hasOwn(PLATFORM_OPERATIONS, operation)) {
      return await this.requestPlatform(operation as PlatformOperationId, params as PlatformOperationParams<PlatformOperationId>, options) as ClientOperationResult<M>;
    }
    const runtimeOperation = operation as OperationId;
    if (!Object.hasOwn(OPERATION_INFO, runtimeOperation)) throw new TypeError(`Unknown operation: ${operation}`);
    if (OPERATION_INFO[runtimeOperation].streaming) throw new LightspeedTransportError("Use watch() for universe/watch");
    const response = await this.request(runtimeOperation, params as OperationParams<OperationId>, options);
    if (HTTP_OPERATIONS[runtimeOperation].transfer === "binaryDownload") {
      try { return new Uint8Array(await response.arrayBuffer()) as ClientOperationResult<M>; }
      catch (cause) {
        options.signal?.throwIfAborted();
        throw new LightspeedTransportError("Download interrupted", { cause });
      }
    }
    return await this.readBody(response, options.signal) as ClientOperationResult<M>;
  }

  /** Native account, application, credential, and universe management on Platform. */
  private async requestPlatform(operation: PlatformOperationId, params: PlatformOperationParams<PlatformOperationId>, options: CallOptions): Promise<unknown> {
    const spec = PLATFORM_OPERATIONS[operation];
    const body = { ...params } as Record<string, unknown>;
    const path = spec.path.replace(/\{([^}]+)\}/g, (_, field: string) => {
      const value = body[field];
      delete body[field];
      if (typeof value !== "string" || !value || value === "." || value === "..") throw new TypeError(`Invalid path parameter ${field}`);
      return encodeURIComponent(value);
    });
    if (spec.method === "GET" && Object.keys(body).length) throw new TypeError("Unexpected query parameters");
    const headers = await this.buildHeaders(options.headers);
    headers.set("content-type", "application/json");
    const response = await this.send(this.endpoint + path.slice("/api/v1".length), {
      method: spec.method, headers, ...(options.signal ? { signal: options.signal } : {}),
      ...(spec.method === "GET" || !Object.keys(body).length ? {} : { body: JSON.stringify(body) }),
    }, options.signal);
    return await this.readBody(response, options.signal);
  }

  /** Open a download without buffering it. The caller consumes or cancels its body. */
  openDownload<M extends "blobs/read" | "vfs/workspaces/files/read">(
    operation: M, params: OperationParams<M>, options: CallOptions = {},
  ): Promise<Response> {
    return this.request(operation, params, options);
  }

  /** Upload content-addressed bytes, checking the expected digest in the runtime. */
  uploadBlob(blobRef: string, body: Uint8Array | ReadableStream<Uint8Array>, options: CallOptions = {}) {
    return this.call("blobs/upload", { blobRef }, { ...options, body });
  }

  private async request<M extends OperationId>(operation: M, params: OperationParams<M>, options: CallOptions): Promise<Response> {
    const request = prepareHttpRequest(operation, params, {
      endpoint: this.endpoint, ...(this.options.universe === undefined ? {} : { universe: this.options.universe }),
    });
    const headers = await this.buildHeaders(options.headers);
    const transfer = HTTP_OPERATIONS[operation].transfer;
    headers.set("accept", transfer === "sse" ? "text/event-stream" : transfer === "binaryDownload" ? "application/octet-stream" : "application/json");
    const init: RequestInit & { duplex?: "half" } = { method: request.method, headers };
    if (options.signal) init.signal = options.signal;
    if (transfer === "binaryUpload") {
      const body = options.body;
      if (!body) throw new TypeError("A binary upload requires body bytes or a stream");
      const length = body instanceof Uint8Array ? body.byteLength : options.contentLength;
      if (length === undefined || !Number.isSafeInteger(length) || length < 0) throw new TypeError("A binary upload requires a valid contentLength");
      headers.set("content-type", "application/octet-stream");
      headers.set("content-length", String(length));
      init.body = body instanceof Uint8Array ? new Uint8Array(body) : body;
      if (body instanceof ReadableStream) init.duplex = "half";
    } else if (request.body !== undefined) {
      headers.set("content-type", "application/json");
        init.body = JSON.stringify(request.body);
    }
    return this.send(request.url, init, options.signal);
  }

  private async send(url: string | URL, init: RequestInit, signal?: AbortSignal): Promise<Response> {
    let response: Response;
    try {
      const { fetchImpl } = this;
      response = await fetchImpl(url, { ...init, redirect: "manual" });
    } catch (cause) {
      signal?.throwIfAborted();
      throw new LightspeedTransportError(`Lightspeed API request failed: ${errorMessage(cause)}`, { cause });
    }
    const expected = new URL(url);
    if (response.redirected || response.status >= 300 && response.status < 400 || response.type === "opaqueredirect" ||
        (response.url && new URL(response.url).origin !== expected.origin)) {
      await response.body?.cancel();
      throw new LightspeedTransportError("Lightspeed API redirects are not allowed");
    }
    const rotated = response.headers.get("set-auth-token");
    const authorization = new Headers(init.headers).get("authorization");
    if (rotated && this.options.onSessionToken && authorization?.startsWith("Bearer ")) {
      if (rotated.length > 4096 || /[^\x21-\x7e]/.test(rotated)) { await response.body?.cancel(); throw new LightspeedTransportError("Invalid renewed session credential"); }
      try { await this.options.onSessionToken(rotated, authorization.slice(7)); }
      catch (error) { await response.body?.cancel(); throw error; }
    }
    if (!response.ok) await this.readBody(response, signal);
    return response;
  }

  /**
   * Starts a run. A submission ID is generated if you give none, but only one you
   * keep lets a retry find the same run after a lost response.
   */
  async startRun(
    sessionId: string,
    input: string | InputItem[] | RunStartSource,
    options: StartRunOptions = {},
  ): Promise<RunStartResponse> {
    const { signal: _signal, headers: _headers, body: _body, contentLength: _length, ...fields } = options;
    const params: RunStartParams = {
      ...fields,
      sessionId,
      source: typeof input === "string" || Array.isArray(input)
        ? { type: "input", items: inputItems(input) } : input,
      submissionId: options.submissionId === undefined ? generateSubmissionId() : options.submissionId,
    };
    const result = await this.call("session/runs/start", params, callOptions(options));
    return result;
  }

  /** Sends input to a run in progress, which its next model turn receives. */
  async steerRun(
    sessionId: string,
    runId: string,
    input: string | InputItem[],
    options: SteerRunOptions = {},
  ): Promise<RunSteerResponse> {
    const { signal: _signal, headers: _headers, body: _body, contentLength: _length, ...fields } = options;
    const params: RunSteerParams = {
      ...fields, sessionId, runId, items: inputItems(input),
      submissionId: options.submissionId === undefined ? generateSubmissionId() : options.submissionId,
    };
    const result = await this.call("session/runs/steer", params, callOptions(options));
    return result;
  }

  async readEvents(
    sessionId: string,
    options: ReadEventsOptions = {},
  ): Promise<SessionEventsReadResponse> {
    const { signal: _signal, headers: _headers, body: _body, contentLength: _length, ...fields } = options;
    const params: SessionEventsReadParams = { ...fields, sessionId };
    const result = await this.call("session/events/read", params, callOptions(options));
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
      const result = await read(0, (call) =>
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
        const result = await read(0, (call) =>
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
          throw new LightspeedApiError({
            type: "about:blank", title: "Session execution stopped", status: 409,
            detail: message, kind: "sessionExecutionStopped",
          });
        }
      }
      currentWaitMs = waitMs;
    }
  }

  /** A blob's bytes. Refs come from run output, tool calls, and message entries. */
  async readBlobBytes(blobRef: string, options: CallOptions = {}): Promise<Uint8Array> {
    return this.call("blobs/read", { blobRef }, options);
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
    const result = await this.call(
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
      if (isRecord(body) && typeof body.kind === "string" && typeof body.detail === "string" &&
          typeof body.title === "string" && typeof body.type === "string" && body.status === response.status) {
        throw new LightspeedApiError(body as unknown as HttpProblem);
      }
      throw new LightspeedTransportError(
        `Lightspeed API request failed with HTTP ${response.status}`,
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
    headers.set("accept", "application/json");
    const token = typeof this.options.token === "function" ? await this.options.token() : this.options.token;
    if (token !== undefined) headers.set("authorization", `Bearer ${token}`);
    if (this.options.actor !== undefined) {
      headers.set("x-lightspeed-actor", this.options.actor);
    }
    const defaultHeaders =
      typeof this.headers === "function" ? await this.headers() : this.headers;
    appendHeaders(headers, defaultHeaders);
    appendHeaders(headers, callHeaders);
    if (headers.has("x-lightspeed-universe")) throw new TypeError("Select the universe with the client universe option");
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
