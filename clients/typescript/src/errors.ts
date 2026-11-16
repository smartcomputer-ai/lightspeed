import type { HttpProblem } from "./generated/types.js";

/** A structured failure returned by the HTTP API. */
export class LightspeedApiError extends Error {
  readonly status: number;
  readonly kind: HttpProblem["kind"];
  readonly data: HttpProblem;
  readonly payload: HttpProblem;

  constructor(payload: HttpProblem) {
    super(payload.detail);
    this.name = "LightspeedApiError";
    this.status = payload.status;
    this.kind = payload.kind;
    this.data = payload;
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
