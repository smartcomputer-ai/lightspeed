import type { AgentApiError, JsonRpcError as ApiJsonRpcError } from "./generated/types.js";

export type JsonRpcErrorPayload = ApiJsonRpcError;

export type LightspeedRpcErrorKind =
  | "invalidRequest"
  | "methodNotFound"
  | "unauthenticated"
  | "forbidden"
  | "notFound"
  | "conflict"
  | "rejected"
  | "environmentNotReady"
  | "modelDefaultUnset"
  | "operationPending"
  | "sessionExecutionStopped"
  | "internal"
  | "unknown";

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
    case -32012:
      return "environmentNotReady";
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
    this.kind = lightspeedRpcErrorKind(payload.code);
    this.payload = payload;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export class LightspeedTransportError extends Error {
  readonly status: number | undefined;
  readonly body: unknown | undefined;

  constructor(message: string, options: { status?: number; body?: unknown } = {}) {
    super(message);
    this.name = "LightspeedTransportError";
    this.status = options.status;
    this.body = options.body;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}
