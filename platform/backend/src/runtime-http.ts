import { Hono, type Context } from "hono";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { LightspeedApiError, type ConnectionResponse, type UniverseWatchParams } from "@lightspeed-ai/sdk";
import { HTTP_CONTRACT_REVISION, HTTP_OPERATIONS } from "@lightspeed-ai/sdk/server";
import type { AppContext, ApiVariables } from "./context.js";
import { isPlatformAdmin } from "./context.js";
import { clientOptions, deploymentClient, GateRefusal, memberClient } from "./runtime-client.js";
import { ADMIN_OPERATIONS, MEMBER_OPERATIONS, NATIVE_OPERATIONS } from "./runtime-admission.js";
import { createUniverseRows, deleteManagedUniverse, universeForRuntimeReference } from "./routes/universes.js";
import { requireAvailableCachedHandle } from "./universe-handles.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
const MAX_POLICY_BODY = 1024 * 1024;

/** Only explicitly reviewed operations become routes. The runtime contract owns
 * every verb, path and field location; Platform owns admission and identity. */
export function runtimeHttpRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.onError((error, c) => {
    if (error instanceof GateRefusal) return problem(c, error.status, error.status === 404 ? "notFound" : "forbidden", error.message);
    if (error instanceof LightspeedApiError) return new Response(JSON.stringify(error.payload), { status: error.status, headers: { "content-type": "application/problem+json" } });
    console.error("runtime API request failed", error);
    return problem(c, 502, "internal", "runtime unavailable");
  });
  for (const spec of Object.values(HTTP_OPERATIONS)) {
    if (![MEMBER_OPERATIONS, ADMIN_OPERATIONS, NATIVE_OPERATIONS].some(set => set.has(spec.operationId))) continue;
    const path = spec.path.replace(/^\/api\/v1/, "").replace(/\{([^}]+)\}/g, ":$1");
    app.on(spec.method, path, async c => {
      if (c.req.method !== spec.method) return problem(c, 405, "invalidRequest", "method not allowed");
      const requestedRevision = c.req.header("x-lightspeed-contract-revision");
      if (requestedRevision && requestedRevision !== HTTP_CONTRACT_REVISION) return problem(c, 409, "invalidRequest", "unsupported API contract revision");
      const connection = await deploymentClient(ctx.env).call("initialize", {}, { signal: c.req.raw.signal });
      if (connection.contractRevision !== HTTP_CONTRACT_REVISION || connection.runtime !== "runtime") return problem(c, 502, "invalidRequest", "runtime API contract does not match Platform");
      if (NATIVE_OPERATIONS.has(spec.operationId)) return nativeOperation(ctx, c, spec.operationId, connection);
      const options = clientOptions(ctx.env, undefined, { "x-lightspeed-actor": c.get("session").user.id });
      const url = new URL(options.endpoint);
      const incoming = new URL(c.req.url);
      let upstreamPath: string = spec.path;
      let params: Record<string, unknown> = {};
      const reference = c.req.param("universe");
      if (MEMBER_OPERATIONS.has(spec.operationId)) {
        const access = await universeForRuntimeReference(ctx, c, reference ?? "");
        if (!access) throw new GateRefusal(404, "universe not found");
        // Recheck the stored destination too; no database value can redirect the token.
        clientOptions(ctx.env, access.universe.gatewayUrl, {});
        upstreamPath = upstreamPath.replace("{universe}", encodeURIComponent(access.universe.lightspeedUniverseId));
        const member = memberClient(ctx.env, access.universe, access.member);
        for (const field of spec.pathFields) setField(params, field.field, c.req.param(field.parameter));
        let body: string | undefined;
        if (member.needsPolicyBody(spec.operationId)) {
          const parsed = await policyBody(c.req.raw);
          if (spec.pathFields.some(field => ownsField(parsed, field.field))) return problem(c, 400, "invalidRequest", "path-owned field must not appear in the body");
          params = { ...parsed, ...params };
          const decision = await member.authorizeOperation(spec.operationId, params, { signal: c.req.raw.signal });
          if (decision.response !== undefined) return c.json(decision.response);
          const authorized = decision.params;
          for (const field of spec.pathFields) deleteField(authorized, field.field);
          body = JSON.stringify(authorized);
        } else {
          const decision = await member.authorizeOperation(spec.operationId, params, { signal: c.req.raw.signal });
          if (decision.response !== undefined) return c.json(decision.response);
          const authorized = decision.params;
          if (spec.operationId === "session/list" && authorized.visibleTo) incoming.searchParams.set("visibleTo", String(authorized.visibleTo));
          if (spec.operationId === "session/delete") {
            incoming.searchParams.set("softDelete", "true");
            if (authorized.sharedOnly) incoming.searchParams.set("sharedOnly", "true");
          }
        }
        if (spec.operationId === "universe/watch") {
          const kinds = incoming.searchParams.getAll("kinds");
          if (kinds.some(kind => !["session", "bot", "environment", "workspace"].includes(kind))) return problem(c, 400, "invalidRequest", "invalid watch kind");
          const watch = member.authorizeWatch({ ...(kinds.length ? { kinds: kinds as UniverseWatchParams["kinds"] } : {}), visibleTo: incoming.searchParams.get("visibleTo") });
          incoming.searchParams.delete("kinds");
          for (const kind of watch.kinds ?? []) incoming.searchParams.append("kinds", kind);
          incoming.searchParams.delete("visibleTo");
          if (watch.visibleTo) incoming.searchParams.set("visibleTo", watch.visibleTo);
        }
        for (const field of spec.pathFields) upstreamPath = upstreamPath.replace(`{${field.parameter}}`, encodeURIComponent(c.req.param(field.parameter)!));
        url.pathname = url.pathname.replace(/\/$/, "") + upstreamPath.replace(/^\/api\/v1/, "");
        url.search = incoming.search;
        return forward(c.req.raw, url, options.headers, body, spec.transfer === "sse");
      }
      if (!isPlatformAdmin(c.get("session"))) throw new GateRefusal(403, "platform admin required");
      // Deployment routes with a universe still resolve a Platform-owned resource.
      if (reference) {
        const access = await universeForRuntimeReference(ctx, c, reference);
        if (!access) throw new GateRefusal(404, "universe not found");
        upstreamPath = upstreamPath.replace("{universe}", encodeURIComponent(access.universe.lightspeedUniverseId));
      }
      for (const field of spec.pathFields) upstreamPath = upstreamPath.replace(`{${field.parameter}}`, encodeURIComponent(c.req.param(field.parameter)!));
      url.pathname = url.pathname.replace(/\/$/, "") + upstreamPath.replace(/^\/api\/v1/, "");
      url.search = incoming.search;
      return forward(c.req.raw, url, options.headers);
    });
  }
  app.all("*", c => problem(c, 404, "notFound", "operation not exposed by Platform"));
  return app;
}

export function problem(_c: ApiContext, status: number, kind: string, detail: string) {
  return new Response(JSON.stringify({ type: "about:blank", title: detail, detail, status, kind }), { status, headers: { "content-type": "application/problem+json" } });
}

/** Body inspection is an explicit policy exception. Reject lossy JS numeric
 * values before rewriting, rather than silently rounding a Rust integer. */
export async function policyBody(request: Request): Promise<Record<string, unknown>> {
  const declared = Number(request.headers.get("content-length"));
  if (declared > MAX_POLICY_BODY) throw new GateRefusal(403, "policy-inspected body exceeds 1 MiB");
  const reader = request.body?.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  if (reader) try {
    for (;;) {
      const next = await reader.read();
      if (next.done) break;
      size += next.value.byteLength;
      if (size > MAX_POLICY_BODY) { await reader.cancel(); throw new GateRefusal(403, "policy-inspected body exceeds 1 MiB"); }
      chunks.push(next.value);
    }
  } finally { reader.releaseLock(); }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes), (_key, v: unknown) => {
      if (typeof v === "number" && (!Number.isFinite(v) || (Number.isInteger(v) && !Number.isSafeInteger(v)))) throw new Error("unsafe numeric value");
      return v;
    });
  } catch { throw new GateRefusal(403, "policy-inspected body must be valid JSON with lossless numbers"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new GateRefusal(403, "policy-inspected body must be an object");
  return value as Record<string, unknown>;
}

function ownsField(object: Record<string, unknown>, field: string): boolean {
  const parts = field.split("."); const key = parts.pop()!;
  for (const part of parts) { const next = object[part]; if (!next || typeof next !== "object") return false; object = next as Record<string, unknown>; }
  return Object.hasOwn(object, key);
}
function setField(object: Record<string, unknown>, field: string, value: unknown) {
  const parts = field.split(".");
  const key = parts.pop()!;
  for (const part of parts) object = (object[part] ??= {}) as Record<string, unknown>;
  object[key] = value;
}
function deleteField(object: Record<string, unknown>, field: string) {
  const parts = field.split("."); const key = parts.pop()!;
  for (const part of parts) { const next = object[part]; if (!next || typeof next !== "object") return; object = next as Record<string, unknown>; }
  delete object[key];
}

/** Web streams propagate demand and cancellation; identity and hop-by-hop
 * headers never cross from the caller to the trusted runtime connection. */
async function forward(request: Request, url: URL, identity: Record<string, string>, rewrittenBody?: string, watch = false): Promise<Response> {
  const abort = new AbortController();
  const cancel = () => abort.abort();
  request.signal.addEventListener("abort", cancel, { once: true });
  if (request.signal.aborted) cancel();
  const timer = watch ? setTimeout(cancel, 60_000) : undefined;
  const cleanup = () => { if (timer) clearTimeout(timer); request.signal.removeEventListener("abort", cancel); };
  const headers = new Headers(identity);
  headers.set("accept-encoding", "identity");
  for (const name of ["content-type", "accept", "if-match", "if-none-match", "last-event-id"]) {
    const value = request.headers.get(name); if (value) headers.set(name, value);
  }
  if (rewrittenBody === undefined && request.headers.has("content-length")) headers.set("content-length", request.headers.get("content-length")!);
  if (rewrittenBody !== undefined) headers.set("content-type", "application/json");
  try {
    const upstream = await fetch(url, { method: request.method, headers, body: rewrittenBody ?? request.body, signal: abort.signal, redirect: "error", duplex: "half" } as RequestInit);
    const outputHeaders = new Headers({ "x-lightspeed-contract-revision": HTTP_CONTRACT_REVISION });
    for (const name of ["content-type", "content-length", "cache-control", "etag", "content-disposition", "retry-after"]) {
      const value = upstream.headers.get(name); if (value) outputHeaders.set(name, value);
    }
    if (watch) { outputHeaders.set("cache-control", "no-store"); outputHeaders.set("x-accel-buffering", "no"); }
    if (!upstream.body) { cleanup(); return new Response(null, { status: upstream.status, headers: outputHeaders }); }
    const reader = upstream.body.getReader();
    const body = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try { const next = await reader.read(); if (next.done) { cleanup(); reader.releaseLock(); controller.close(); } else controller.enqueue(next.value); }
        catch (error) { cleanup(); reader.releaseLock(); controller.error(error); cancel(); }
      },
      async cancel() { cancel(); cleanup(); await reader.cancel().catch(() => {}); reader.releaseLock(); },
    });
    return new Response(body, { status: upstream.status, headers: outputHeaders });
  } catch (error) { cancel(); cleanup(); throw error; }
}

async function nativeOperation(ctx: AppContext, c: ApiContext, operation: string, connection: ConnectionResponse): Promise<Response> {
  const actorId = c.get("session").user.id;
  const client = deploymentClient(ctx.env);
  if (operation === "initialize") return c.json({ ...connection, runtime: "platform", caller: { single: false } });
  if (operation === "deployment/universes/list") {
    const listed = await client.call("deployment/universes/list", {});
    const allowed = await Promise.all((listed.universes ?? []).map(async universe => { const access = await universeForRuntimeReference(ctx, c, universe.universeId); return access ? visibleUniverse(universe, access.role) : null; }));
    return c.json({ universes: allowed.filter(universe => universe !== null) });
  }
  if (operation === "deployment/universes/create") {
    if (!isPlatformAdmin(c.get("session"))) throw new GateRefusal(403, "platform admin required");
    const body = await policyBody(c.req.raw);
    if (typeof body.universeId !== "string" || (body.handle != null && typeof body.handle !== "string") || Object.keys(body).some(key => !["universeId", "handle"].includes(key))) return problem(c, 400, "invalidRequest", "expected universeId and optional handle");
    const existing = await universeForRuntimeReference(ctx, c, body.universeId);
    if (body.handle && !existing) await requireAvailableCachedHandle(ctx, body.handle as string);
    const result = await client.call("deployment/universes/create", { universeId: body.universeId, handle: body.handle as string | undefined });
    if (!existing) await createUniverseRows(ctx, actorId, result.universe.handle, result.universe.handle, result.universe.universeId);
    return c.json(result, result.created ? 201 : 200);
  }
  const access = await universeForRuntimeReference(ctx, c, c.req.param("universe") ?? "", operation === "deployment/universes/delete");
  if (!access) throw new GateRefusal(404, "universe not found");
  if (operation === "deployment/universes/read") { const result = await client.call("deployment/universes/read", { universe: access.universe.lightspeedUniverseId }); return c.json({ universe: visibleUniverse(result.universe, access.role) }); }
  if (operation === "deployment/universes/delete") {
    if (!isPlatformAdmin(c.get("session"))) throw new GateRefusal(403, "platform admin required");
    return c.json(await deleteManagedUniverse(ctx, access, actorId));
  }
  if (access.role !== "admin") throw new GateRefusal(403, "universe admin required");
  const body = await policyBody(c.req.raw);
  if (typeof body.handle !== "string" || Object.keys(body).some(key => key !== "handle")) return problem(c, 400, "invalidRequest", "expected handle");
  await requireAvailableCachedHandle(ctx, body.handle, access.universe.organizationId);
  const result = await client.call("deployment/universes/handle/put", { universe: access.universe.lightspeedUniverseId, handle: body.handle });
  await ctx.db.update(schema.organization).set({ slug: result.universe.handle }).where(eq(schema.organization.id, access.universe.organizationId));
  return c.json(result);
}

function visibleUniverse<T extends { members?: unknown; directoryGroupMappings?: unknown }>(universe: T, role: string): T {
  if (role === "admin") return universe;
  const { members: _members, directoryGroupMappings: _groups, ...view } = universe;
  return view as T;
}
