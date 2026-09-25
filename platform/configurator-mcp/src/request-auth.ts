import type { IncomingHttpHeaders } from "node:http";

export const UNIVERSE_HEADER = "x-lightspeed-universe";
export const ACTOR_HEADER = "x-lightspeed-actor";

export type ConfiguratorAuthMode = "single" | "authenticated";
export type RequestAuthContext =
  | { mode: "single" }
  | { mode: "authenticated"; apiKey: string; universeId?: string; actor?: string };

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
      rejectHeader(headers, ACTOR_HEADER, mode);
      rejectHeader(headers, "x-lightspeed-principal", mode);
      return { mode };
    case "authenticated": {
      const authorization = requiredHeader(headers, "authorization");
      const apiKey = /^Bearer (lsk_\S+)$/.exec(authorization)?.[1];
      if (!apiKey) throw new HttpAuthError(401, "Authorization must contain a Lightspeed bearer API key");
      const universeId = optionalHeader(headers, UNIVERSE_HEADER);
      rejectHeader(headers, "x-lightspeed-principal", mode);
      const actor = optionalHeader(headers, ACTOR_HEADER);
      if (universeId !== undefined && !UUID_PATTERN.test(universeId)) throw new HttpAuthError(400, `invalid ${UNIVERSE_HEADER}`);
      if (actor !== undefined && (actor.length > 256 || [...actor].some((char) => /\p{Cc}/u.test(char)))) throw new HttpAuthError(400, `invalid ${ACTOR_HEADER}`);
      // Runtime validates the credential, status, scope and assertion capability.
      return { mode, apiKey, ...(universeId ? { universeId } : {}), ...(actor ? { actor } : {}) };
    }
  }
}

export function upstreamHeaders(auth: RequestAuthContext): HeadersInit | undefined {
  switch (auth.mode) {
    case "single":
      return undefined;
    case "authenticated":
      return { authorization: `Bearer ${auth.apiKey}`,
        ...(auth.universeId ? { [UNIVERSE_HEADER]: auth.universeId } : {}),
        ...(auth.actor ? { [ACTOR_HEADER]: auth.actor } : {}) };

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

const UUID_PATTERN = /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i;
