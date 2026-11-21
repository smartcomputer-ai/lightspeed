import { LightspeedApiError, LightspeedTransportError } from "@lightspeed-ai/sdk";
import { WorkspaceTransferError } from "@lightspeed-ai/platform-shared";
import { UniverseHandleCacheConflict } from "./universe-handles.js";
import { RuntimeUnconfigured, GateRefusal } from "./runtime-client.js";
import type { Context } from "hono";
import { HTTPException } from "hono/http-exception";
import { routePath } from "hono/route";
import type { ZodType } from "zod";

/// Parses and validates a JSON body, returning a 400 response on failure.
export async function parseBody<T>(c: Context, schema: ZodType<T>): Promise<
  { ok: true; data: T } | { ok: false; response: Response }
> {
  let raw: unknown;
  try {
    raw = await c.req.json();
  } catch {
    return { ok: false, response: c.json({ error: "invalid JSON body" }, 400) };
  }
  const parsed = schema.safeParse(raw);
  if (!parsed.success) {
    return {
      ok: false,
      response: c.json(
        { error: "validation failed", issues: parsed.error.issues },
        400,
      ),
    };
  }
  return { ok: true, data: parsed.data };
}

/// Maps runtime failures onto API responses: notFound/conflict pass
/// through as 404/409, transport or internal errors surface as 502. Only
/// typed errors carry their message; anything else (a database driver error
/// includes its SQL and parameters) is logged here and answered generically.
export async function withApiErrors(
  c: Context,
  fn: () => Promise<Response>,
): Promise<Response> {
  try {
    return await fn();
  } catch (error) {
    const signal = c.req.raw.signal;
    if (signal.aborted && error === signal.reason) return new Response(null, { status: 499 });
    // Preserve the framework status and let the app choose its UI/problem shape.
    if (error instanceof HTTPException) throw error;
    if (error instanceof WorkspaceTransferError) return c.json({ error: error.message }, error.status);
    if (error instanceof UniverseHandleCacheConflict) return c.json({ error: error.message }, 409);
    if (error instanceof RuntimeUnconfigured) {
      return c.json({ error: error.message }, 501);
    }
    if (error instanceof LightspeedTransportError) {
      console.warn("runtime transport failure", { method: c.req.method, route: routePath(c), upstreamStatus: error.status });
      return c.json({ error: "runtime unavailable" }, 502);
    }
    if (error instanceof GateRefusal) {
      return c.json({ error: error.status === 404 ? "not found" : error.message }, error.status);
    }
    const code = (error as { cause?: { code?: string } } | null)?.cause?.code;
    if (code === "23505") return c.json({ error: "record already exists" }, 409);
    if (error instanceof LightspeedApiError) {
      if (error.kind === "modelDefaultUnset") return c.json({ error: error.message, kind: error.kind, modelDefaultSlot: error.data?.modelDefaultSlot }, 400);
      if (error.kind === "operationPending") return c.json({ error: error.message, kind: error.kind, submissionId: error.data?.submissionId }, 409);
      if (error.kind === "invalidRequest" || error.kind === "audioBlobTooLarge") return c.json({ error: error.message }, 400);
      // Core refusing the Platform means its own runtime token or gate is wrong: the
      // caller was already admitted here.
      if (error.kind === "unauthenticated") return c.json({ error: "runtime rejected the Platform token" }, 502);
      if (error.kind === "forbidden") return c.json({ error: "runtime refused the Platform" }, 500);
      if (error.kind === "rejected") return c.json({ error: error.message }, 409);
      if (error.kind === "sessionExecutionStopped") return c.json({ error: error.message, kind: error.kind }, 409);
      if (error.kind === "gone") return c.json({ error: error.message, kind: error.kind }, 410);
      if (error.kind === "notFound") {
        return c.json({ error: "not found in runtime" }, 404);
      }
      if (error.kind === "conflict") {
        return c.json({ error: `runtime conflict: ${error.message}` }, 409);
      }
      return c.json({ error: `runtime error: ${error.message}` }, 502);
    }
    console.error("unhandled request error", error);
    return c.json({ error: "internal error" }, 502);
  }
}
