import type {
  AgentApiError,
  AgentApiErrorKind,
  JsonRpcError as ApiJsonRpcError,
} from "./generated/types.js";

export type JsonRpcErrorPayload = ApiJsonRpcError;

export type LightspeedRpcErrorKind = AgentApiErrorKind | "methodNotFound" | "unknown";

export function lightspeedRpcErrorKind(code: number): LightspeedRpcErrorKind {
  switch (code) {
    case -32602:
      return "invalidRequest";
    case -32601:
      return "methodNotFound";
    case -32001:
      return "unauthenticated";
    case -32003:
      return "forbidden";
    case -32004:
      return "notFound";
    case -32009:
      return "conflict";
    case -32010:
      return "rejected";
    case -32011:
      return "sessionBootstrapFailed";
    case -32012:
      return "environmentNotReady";
    case -32013:
      return "responseTooLarge";
    case -32603:
      return "internal";
    case -32014:
      return "modelDefaultUnset";
    case -32015:
      return "operationPending";
    case -32016:
      return "sessionExecutionStopped";
    default:
      return "unknown";
  }
}

export class LightspeedRpcError extends Error {
  readonly code: number;
  readonly data: AgentApiError | null | undefined;
  readonly kind: LightspeedRpcErrorKind;
  readonly payload: JsonRpcErrorPayload;

  constructor(payload: JsonRpcErrorPayload) {
    super(payload.message);
    this.name = "LightspeedRpcError";
    this.code = payload.code;
    this.data = payload.data;
    // Several kinds can share a code, so the server's own kind comes first.
    this.kind = payload.data?.kind ?? lightspeedRpcErrorKind(payload.code);
    this.payload = payload;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export class LightspeedTransportError extends Error {
  readonly status: number | undefined;
  readonly body: unknown | undefined;

  constructor(message: string, options: { status?: number; body?: unknown; cause?: unknown } = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "LightspeedTransportError";
    this.status = options.status;
    this.body = options.body;
    Object.setPrototypeOf(this, new.target.prototype);
  }

  /** The request may succeed if repeated: the network failed, or the server or a proxy is struggling. */
  get transient(): boolean {
    if (this.status === undefined) {
      return this.cause !== undefined;
    }
    return this.status >= 500 || this.status === 408 || this.status === 429;
  }
}
