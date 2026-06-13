export interface JsonRpcErrorPayload {
  code: number;
  message: string;
  data?: unknown;
}

export type LightspeedRpcErrorKind =
  | "invalid_request"
  | "method_not_found"
  | "not_found"
  | "conflict"
  | "rejected"
  | "internal"
  | "unknown";

export function lightspeedRpcErrorKind(code: number): LightspeedRpcErrorKind {
  switch (code) {
    case -32602:
      return "invalid_request";
    case -32601:
      return "method_not_found";
    case -32004:
      return "not_found";
    case -32009:
      return "conflict";
    case -32010:
      return "rejected";
    case -32603:
      return "internal";
    default:
      return "unknown";
  }
}

export class LightspeedRpcError extends Error {
  readonly code: number;
  readonly data: unknown | undefined;
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
