import type { IncomingHttpHeaders } from "node:http";

export const UNIVERSE_HEADER = "x-lightspeed-universe";
export const PRINCIPAL_HEADER = "x-lightspeed-principal";

export type ConfiguratorAuthMode = "single" | "trusted-header" | "api-key";

export type RequestAuthContext =
  | { mode: "single" }
  | { mode: "trusted-header"; universeId: string; principal?: string }
  | { mode: "api-key"; apiKey: string };

export class HttpAuthError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "HttpAuthError";
    this.status = status;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export function authenticateHeaders(
  mode: ConfiguratorAuthMode,
  headers: IncomingHttpHeaders,
): RequestAuthContext {
  switch (mode) {
    case "single":
      rejectHeader(headers, "authorization", mode);
      rejectHeader(headers, UNIVERSE_HEADER, mode);
      rejectHeader(headers, PRINCIPAL_HEADER, mode);
      return { mode };
    case "trusted-header": {
      rejectHeader(headers, "authorization", mode);
      const universeId = requiredHeader(headers, UNIVERSE_HEADER);
      if (!UUID_PATTERN.test(universeId)) {
        throw new HttpAuthError(400, `invalid ${UNIVERSE_HEADER} header`);
      }
      const principal = optionalHeader(headers, PRINCIPAL_HEADER);
      if (principal !== undefined) {
        validatePrincipal(principal);
        return { mode, universeId, principal };
      }
      return { mode, universeId };
    }
    case "api-key": {
      rejectHeader(headers, UNIVERSE_HEADER, mode);
      rejectHeader(headers, PRINCIPAL_HEADER, mode);
      const authorization = requiredHeader(headers, "authorization");
      const match = /^Bearer\s+(\S+)$/i.exec(authorization);
      const apiKey = match?.[1];
      if (!apiKey || !apiKey.startsWith("lsk_")) {
        throw new HttpAuthError(401, "Authorization must contain a Lightspeed bearer API key");
      }
      return { mode, apiKey };
    }
  }
}

export function upstreamHeaders(auth: RequestAuthContext): HeadersInit | undefined {
  switch (auth.mode) {
    case "single":
      return undefined;
    case "trusted-header": {
      const headers: Record<string, string> = { [UNIVERSE_HEADER]: auth.universeId };
      if (auth.principal !== undefined) {
        headers[PRINCIPAL_HEADER] = auth.principal;
      }
      return headers;
    }
    case "api-key":
      return { authorization: `Bearer ${auth.apiKey}` };
  }
}

function requiredHeader(headers: IncomingHttpHeaders, name: string): string {
  const value = optionalHeader(headers, name);
  if (value === undefined) {
    throw new HttpAuthError(401, `missing required ${name} header`);
  }
  return value;
}

function optionalHeader(headers: IncomingHttpHeaders, name: string): string | undefined {
  const raw = headers[name];
  if (raw === undefined) {
    return undefined;
  }
  if (Array.isArray(raw)) {
    throw new HttpAuthError(400, `${name} must appear exactly once`);
  }
  const value = raw.trim();
  if (value.length === 0) {
    throw new HttpAuthError(400, `${name} must not be empty`);
  }
  return value;
}

function rejectHeader(headers: IncomingHttpHeaders, name: string, mode: string): void {
  if (headers[name] !== undefined) {
    throw new HttpAuthError(400, `${name} is not accepted in ${mode} mode`);
  }
}

function validatePrincipal(value: string): void {
  const separator = value.indexOf(":");
  if (separator === -1) {
    return;
  }
  const kind = value.slice(0, separator);
  const id = value.slice(separator + 1);
  if ((kind !== "user" && kind !== "service_account") || id.length === 0) {
    throw new HttpAuthError(400, `invalid ${PRINCIPAL_HEADER} header`);
  }
}

const UUID_PATTERN = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i;
