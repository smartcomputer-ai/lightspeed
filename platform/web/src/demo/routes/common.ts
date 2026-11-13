/// Helpers shared by the demo route modules.
import type { Context } from "hono";
import { isCredentialOnlyHeader, type RequestHeader } from "@/lib/request-headers";
import type { DemoStore, UniverseState } from "../store";

/// The universe addressed by the `:id` path param (platform universe id).
export function universeFor(store: DemoStore, c: Context): UniverseState | null {
  return store.universe(c.req.param("id"));
}

export function notFound(c: Context, error = "not found") {
  return c.json({ error }, 404);
}

export function conflict(c: Context, error: string) {
  return c.json({ error }, 409);
}

export function badRequest(c: Context, error: string) {
  return c.json({ error }, 400);
}

/// Parses the JSON body, tolerating an empty one.
export async function readBody<T = Record<string, unknown>>(c: Context): Promise<T> {
  try {
    return (await c.req.json()) as T;
  } catch {
    return {} as T;
  }
}

export function nowIso(): string {
  return new Date().toISOString();
}

export function intQuery(c: Context, name: string, fallback: number): number {
  const raw = Number(c.req.query(name));
  return Number.isFinite(raw) && raw > 0 ? Math.floor(raw) : fallback;
}

export const HOUR_MS = 3_600_000;
export const DAY_MS = 86_400_000;

/// Configured request headers as the runtime stores them, for an MCP server
/// and a model endpoint alike: lowercased names, one value per name, a plain
/// value or a grant id, and a credential under every name that carries a
/// secret. `owner` names the header's owner in a refusal.
export function parseRequestHeaders(
  input: unknown,
  owner: string,
): { headers: Record<string, RequestHeader> } | { error: string } {
  const headers: Record<string, RequestHeader> = {};
  if (input === undefined || input === null) return { headers };
  if (typeof input !== "object" || Array.isArray(input)) return { error: `${owner} headers must be an object` };
  for (const [rawName, value] of Object.entries(input as Record<string, unknown>)) {
    const name = rawName.trim().toLowerCase();
    if (name in headers) {
      return { error: `duplicate ${owner} header "${name}"; header names are case-insensitive` };
    }
    const header = (typeof value === "object" && value !== null ? value : {}) as Record<string, unknown>;
    if (header.type === "literal" && typeof header.value === "string") {
      if (isCredentialOnlyHeader(name)) {
        return {
          error: `${owner} header "${name}" carries a secret: store the value as a static token credential and reference it from the header`,
        };
      }
      headers[name] = { type: "literal", value: header.value };
    } else if (header.type === "credential" && typeof header.grantId === "string") {
      headers[name] = { type: "credential", grantId: header.grantId };
    } else {
      return { error: `${owner} header "${name}" needs a literal value or a credential` };
    }
  }
  return { headers };
}
