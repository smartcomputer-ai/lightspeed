import { listOrganizationsForPrincipal, withOwnerNames } from "./organization-access.js";
import type { PlatformConnectionDetails } from "@lightspeed-ai/platform-shared";
import { Hono, type Context } from "hono";
import { LightspeedApiError, type ConnectionResponse, type UniverseWatchParams } from "@lightspeed-ai/sdk";
import { HTTP_CONTRACT_REVISION, HTTP_OPERATIONS } from "@lightspeed-ai/sdk/server";
import type { AppContext, ApiVariables } from "./context.js";
import { deploymentAllows, enforceRestrictions, getAuth } from "./auth-context.js";
import { HTTPException } from "hono/http-exception";
import { clientOptions, deploymentClient, GateRefusal, universeClient } from "./runtime-client.js";
import { ADMIN_OPERATIONS, MEMBER_OPERATIONS, NATIVE_OPERATIONS } from "./runtime-admission.js";
import { deleteManagedUniverse, listUniverseAccesses, renameManagedUniverse, universeForRequest, updateUniverseLifecycle } from "./routes/universes.js";

type ApiContext = Context<{ Variables: ApiVariables }>;
const MAX_POLICY_BODY = 1024 * 1024;

/** Only explicitly reviewed operations become routes. The runtime contract owns
 * every verb, path and field location; Platform owns admission and identity. */
export function runtimeHttpRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.onError((error, c) => {
    if (error instanceof HTTPException) return problem(c, error.status, error.status === 401 ? "unauthenticated" : error.status === 413 ? "invalidRequest" : "forbidden", error.message);
    if (error instanceof GateRefusal) return problem(c, error.status, error.status === 404 ? "notFound" : error.status === 400 ? "invalidRequest" : error.status === 409 ? "conflict" : "forbidden", error.message);
    if (error instanceof LightspeedApiError) return new Response(JSON.stringify(error.payload), { status: error.status, headers: { "content-type": "application/problem+json" } });
    console.error("runtime API request failed", error);
    return problem(c, 502, "internal", "runtime unavailable");
  });
  for (const spec of Object.values(HTTP_OPERATIONS)) {
    if (![MEMBER_OPERATIONS, ADMIN_OPERATIONS, NATIVE_OPERATIONS].some(set => set.has(spec.operationId))) continue;
    const path = spec.path.replace(/^\/api\/v1/, "").replace(/\{([^}]+)\}/g, ":$1");
    app.on(spec.method, path, async c => {
      if (c.req.method !== spec.method) return problem(c, 405, "invalidRequest", "method not allowed");
      const auth = getAuth(c);
      enforceRestrictions(auth, { operation: spec.operationId });
      const requestedRevision = c.req.header("x-lightspeed-contract-revision");
      if (requestedRevision && requestedRevision !== HTTP_CONTRACT_REVISION) return problem(c, 409, "invalidRequest", "unsupported API contract revision");
      const connection = await deploymentClient(ctx.env).call("initialize", {}, { signal: c.req.raw.signal });
      if (connection.contractRevision !== HTTP_CONTRACT_REVISION || connection.runtime !== "runtime") return problem(c, 502, "invalidRequest", "runtime API contract does not match Platform");
      if (NATIVE_OPERATIONS.has(spec.operationId)) return nativeOperation(ctx, c, spec.operationId, connection);
      const options = clientOptions(ctx.env, { "x-lightspeed-actor": auth.actorId });
      const url = new URL(options.endpoint);
      const incoming = new URL(c.req.url);
      let upstreamPath: string = spec.path;
      let params: Record<string, unknown> = {};
      const reference = c.req.param("universe");
      if (MEMBER_OPERATIONS.has(spec.operationId)) {
        const access = await universeForRequest(ctx, c, reference ?? "");
        if (!access) throw new GateRefusal(404, "universe not found");
        enforceRestrictions(auth, { operation: spec.operationId, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: spec.method !== "GET" });
        upstreamPath = upstreamPath.replace("{universe}", encodeURIComponent(access.universe.lightspeedUniverseId));
        const client = universeClient(ctx.env, access.universe, access.caller);
        for (const field of spec.pathFields) setField(params, field.field, c.req.param(field.parameter));
        let body: string | undefined;
        if (client.needsPolicyBody(spec.operationId)) {
          const parsed = await policyBody(c.req.raw);
          if (spec.pathFields.some(field => ownsField(parsed, field.field))) return problem(c, 400, "invalidRequest", "path-owned field must not appear in the body");
          params = { ...parsed, ...params };
          const decision = await client.authorizeOperation(spec.operationId, params, { signal: c.req.raw.signal });
          if (decision.response !== undefined) return c.json(decision.response);
          const authorized = decision.params;
          for (const field of spec.pathFields) deleteField(authorized, field.field);
          body = JSON.stringify(authorized);
        } else {
          const decision = await client.authorizeOperation(spec.operationId, params, { signal: c.req.raw.signal });
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
          const watch = client.authorizeWatch({ ...(kinds.length ? { kinds: kinds as UniverseWatchParams["kinds"] } : {}), visibleTo: incoming.searchParams.get("visibleTo") });
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
      if (!deploymentAllows(auth, "configureDeployment")) throw new GateRefusal(403, "deployment permission required: configureDeployment");
      if (!reference) enforceRestrictions(auth, { operation: spec.operationId, write: spec.method !== "GET" });
      if (!reference && (auth.credential.restrictions.organizationIds || auth.credential.restrictions.universeIds)) throw new GateRefusal(403, "deployment operation is outside credential resource scope");
      // Deployment routes with a universe still resolve a Platform-owned resource.
      if (reference) {
        const access = await universeForRequest(ctx, c, reference);
        if (!access) throw new GateRefusal(404, "universe not found");
        enforceRestrictions(auth, { operation: spec.operationId, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: spec.method !== "GET" });
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

export function problem(_c: unknown, status: number, kind: string, detail: string) {
  return new Response(JSON.stringify({ type: "about:blank", title: detail, detail, status, kind }), { status, headers: { "content-type": "application/problem+json" } });
}

/** Body inspection is an explicit policy exception. Reject lossy JS numeric
 * values before rewriting, rather than silently rounding a Rust integer. */
export async function policyBody(request: Request): Promise<Record<string, unknown>> {
  const declared = Number(request.headers.get("content-length"));
  if (declared > MAX_POLICY_BODY) throw new HTTPException(413, { message: "policy-inspected body exceeds 1 MiB" });
  const reader = request.body?.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  if (reader) try {
    for (;;) {
      const next = await reader.read();
      if (next.done) break;
      size += next.value.byteLength;
      if (size > MAX_POLICY_BODY) { await reader.cancel(); throw new HTTPException(413, { message: "policy-inspected body exceeds 1 MiB" }); }
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
  const auth = getAuth(c);
  const client = deploymentClient(ctx.env);
  if (operation === "initialize") {
    const principal = auth.principal;
    const admitted = await listOrganizationsForPrincipal(ctx, auth);
    const named = await withOwnerNames(ctx.db, admitted.map(access => access.organization));
    const organizations = admitted.map(({ role }, index) => ({ id: named[index]!.id, name: named[index]!.name, role }));
    const selected = auth.credential.restrictions.universeIds;
    const defaultUniverse = selected?.length === 1 && await universeForRequest(ctx, c, selected[0]!) ? selected[0] : undefined;
    const platform: PlatformConnectionDetails = { principal: { kind: principal.kind, id: principal.id, name: principal.kind === "user" ? principal.user.name : principal.serviceUser.name, platformAdmin: principal.platformAdmin }, credential: { kind: auth.credential.kind, id: auth.credential.id }, organizations, restrictions: auth.credential.restrictions };
    return c.json({ ...connection, defaultUniverse, runtime: "platform", caller: { single: false }, platform });
  }
  if (operation === "deployment/universes/create") return problem(c, 400, "invalidRequest", "Create through /api/v1/platform/organizations/{organizationId}/universes to select an organization and initial Owner");
  if (operation === "deployment/universes/list") {
    const query = new URL(c.req.url).searchParams;
    if ([...query.keys()].some(key => key !== "metadata") || query.getAll("metadata").length > 1) return problem(c, 400, "invalidRequest", "unsupported universe list query");
    let metadata: Record<string, string> | undefined;
    if (query.has("metadata")) {
      try { const value: unknown = JSON.parse(query.get("metadata")!); if (!value || typeof value !== "object" || Array.isArray(value) || Object.values(value).some(entry => typeof entry !== "string")) throw new Error(); metadata = value as Record<string, string>; }
      catch { return problem(c, 400, "invalidRequest", "metadata must be a JSON string map"); }
    }
    const listed = await client.call("deployment/universes/list", metadata ? { metadata } : {});
    const allowed = new Set((await listUniverseAccesses(ctx, auth)).map(access => access.universe.lightspeedUniverseId));
    return c.json({ universes: (listed.universes ?? []).filter(universe => allowed.has(universe.universeId)) });
  }
  const access = await universeForRequest(ctx, c, c.req.param("universe") ?? "", operation === "deployment/universes/delete");
  if (!access) throw new GateRefusal(404, "universe not found");
  enforceRestrictions(auth, { operation, organizationId: access.organization.id, universeId: access.universe.lightspeedUniverseId, write: operation !== "deployment/universes/read" });
  if (operation === "deployment/universes/read") return c.json(await client.call(operation, { universe: access.universe.lightspeedUniverseId }));
  if (operation === "deployment/universes/delete") return c.json(await deleteManagedUniverse(ctx, access));
  const body = await policyBody(c.req.raw);
  if (operation === "deployment/universes/handle/put") {
    if (typeof body.handle !== "string" || Object.keys(body).some(key => key !== "handle")) return problem(c, 400, "invalidRequest", "expected handle");
    return c.json((await renameManagedUniverse(ctx, access, body.handle)).result);
  }
  if (operation === "deployment/universes/configuration/put") {
    if (Object.keys(body).some(key => !["displayName", "metadata", "protected"].includes(key)) || (body.displayName !== undefined && typeof body.displayName !== "string") || (body.protected !== undefined && typeof body.protected !== "boolean") || (body.metadata !== undefined && (!body.metadata || typeof body.metadata !== "object" || Array.isArray(body.metadata) || Object.values(body.metadata).some(value => typeof value !== "string")))) return problem(c, 400, "invalidRequest", "expected universe displayName, metadata and protection settings");
    await updateUniverseLifecycle(ctx, access, { ...(body.displayName !== undefined ? { name: body.displayName as string } : {}), ...(body.protected !== undefined ? { protected: body.protected as boolean } : {}), ...(body.metadata !== undefined ? { metadata: body.metadata as Record<string, string> } : {}) }, operation);
    return c.json(await client.call("deployment/universes/read", { universe: access.universe.lightspeedUniverseId }));
  }
  if (operation === "deployment/universes/expiry/put") {
    if (!Object.hasOwn(body, "expiresAtMs") || Object.keys(body).some(key => key !== "expiresAtMs") || (body.expiresAtMs !== null && (!Number.isSafeInteger(body.expiresAtMs) || (body.expiresAtMs as number) < 0))) return problem(c, 400, "invalidRequest", "expected nullable expiresAtMs");
    await updateUniverseLifecycle(ctx, access, { expiresAtMs: body.expiresAtMs as number | null }, operation);
    return c.json(await client.call("deployment/universes/read", { universe: access.universe.lightspeedUniverseId }));
  }
  throw new GateRefusal(404, "native operation not found");
}
