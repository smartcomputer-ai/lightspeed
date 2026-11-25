import { userSubject } from "../test/subject-fixture.js";
import { universeActions, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { Hono } from "hono";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HTTP_OPERATIONS } from "@lightspeed-ai/sdk/server";
import type { AppContext, ApiVariables } from "./context.js";
import type { AuthContext, CredentialRestrictions } from "./auth-context.js";
import { runtimeHttpRoutes } from "./runtime-http.js";
import { ADMIN_OPERATIONS, DENIED_OPERATIONS, MEMBER_OPERATIONS, NATIVE_OPERATIONS } from "./runtime-admission.js";

const mocks = vi.hoisted(() => ({ access: vi.fn(), create: vi.fn(), remove: vi.fn(), handle: vi.fn() }));
vi.mock("./routes/universes.js", () => ({ universeForRequest: mocks.access, listUniverseAccesses: vi.fn(async () => []), deleteManagedUniverse: mocks.remove, renameManagedUniverse: vi.fn(), updateUniverseLifecycle: vi.fn() }));
vi.mock("./universe-handles.js", () => ({ requireAvailableCachedHandle: mocks.handle }));
vi.mock("./organization-access.js", () => ({ listOrganizationsForPrincipal: vi.fn(async () => []), withOwnerNames: vi.fn(async (_db: unknown, organizations: unknown[]) => organizations) }));
const runtimeUniverse = "00000000-0000-0000-0000-000000000001";
const token = "platform-test-token-12345678901234567890";
const connection = { runtime: "runtime", contractRevision: "1", caller: { single: false, tokenName: "platform" }, serverInfo: { name: "Lightspeed", version: "1" } };
const access = (role = "owner") => ({ universe: { id: "platform-id", lightspeedUniverseId: runtimeUniverse, features: {} }, handle: "acme", role, actions: universeActions([role as UniverseRole]), caller: { actorId: "alice", actions: universeActions([role as UniverseRole]) } });

function app(role = "owner", endpoint = "https://runtime.test/api/v1", platformAdmin = false, options: { auth?: AuthContext; organizationRole?: "admin" | "creator" | "member" } = {}) {
  const auth = options.auth ?? userSubject("alice", platformAdmin);
  mocks.access.mockResolvedValue({ ...access(role), auth, organizationRole: options.organizationRole ?? (platformAdmin ? "admin" : "member"), organization: { id: "organization", requireProtected: false } });
  const ctx = { db: { select: () => ({ from: () => ({ where: async () => [], then: (resolve: (rows: unknown[]) => unknown) => Promise.resolve([]).then(resolve) }) }) }, env: { lightspeedApiUrl: endpoint, lightspeedApiToken: token } } as unknown as AppContext;
  const result = new Hono<{ Variables: ApiVariables }>();
  result.use("*", async (c, next) => { c.set("auth", auth); c.set("session", { user: { id: "alice", role: platformAdmin ? "admin" : "user" } } as ApiVariables["session"]); await next(); });
  result.route("/api/v1", runtimeHttpRoutes(ctx));
  return result;
}
function runtime(handler: (url: URL, init: RequestInit) => Response | Promise<Response> = () => Response.json({})) {
  const calls: Array<{ url: URL; init: RequestInit }> = [];
  vi.stubGlobal("fetch", vi.fn(async (input: Parameters<typeof fetch>[0], init: RequestInit = {}) => {
    const url = new URL(String(input));
    calls.push({ url, init });
    if (url.pathname.endsWith("/connection")) return Response.json(connection);
    return handler(url, init);
  }));
  return calls;
}
const path = (suffix: string) => `/api/v1/universes/acme${suffix}`;
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

describe("shared resource API admission", () => {
  it("classifies the complete current catalog explicitly, without overlapping admissions", () => {
    for (const spec of Object.values(HTTP_OPERATIONS)) {
      expect([MEMBER_OPERATIONS, ADMIN_OPERATIONS, NATIVE_OPERATIONS, DENIED_OPERATIONS].filter(set => set.has(spec.operationId)), spec.operationId).toHaveLength(1);
    }
  });
  it("never forwards unknown or service routes", async () => {
    const calls = runtime(); const api = app();
    expect((await api.request(path("/credentials/grants/token/lease"), { method: "POST" })).status).toBe(404);
    expect((await api.request(path("/invented"))).status).toBe(404);
    expect(calls).toHaveLength(0);
  });
  it("enforces operation roles and returns typed HTTP problems", async () => {
    const calls = runtime();
    const response = await app("viewer").request(path("/profiles/main"), { method: "PUT", body: "{}" });
    expect(response.status).toBe(403);
    expect(response.headers.get("content-type")).toBe("application/problem+json");
    expect(await response.json()).toMatchObject({ status: 403, kind: "forbidden", detail: "universe permission required: manageProfile" });
    expect(calls).toHaveLength(1);
  });
  it("streams opaque bodies unchanged, preserves a configured prefix and replaces caller identity", async () => {
    const body = '{"unknown":12345678901234567890,"profile":{"text":"unchanged"}}';
    let forwarded = "";
    const calls = runtime(async (_url, init) => { forwarded = await new Response(init.body).text(); return Response.json({ profile: { profileId: "main" } }); });
    const response = await app("operator", "https://runtime.test/gateway/api/v1").request(path("/profiles/main"), {
      method: "PUT", headers: { "content-type": "application/json", authorization: "Bearer attacker", "x-lightspeed-actor": "mallory", "x-lightspeed-universe": "other", cookie: "secret=1" }, body,
    });
    expect(response.status).toBe(200); expect(forwarded).toBe(body);
    const call = calls.at(-1)!;
    expect(call.url.pathname).toBe(`/gateway/api/v1/universes/${runtimeUniverse}/profiles/main`);
    const headers = new Headers(call.init.headers);
    expect(headers.get("authorization")).toBe(`Bearer ${token}`);
    expect(headers.get("x-lightspeed-actor")).toBe("alice");
    expect(headers.has("x-lightspeed-universe")).toBe(false);
    expect(headers.has("cookie")).toBe(false);
    expect(call.init.redirect).toBe("error");
  });
  it("fails closed when the deployed contract does not match", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ ...connection, contractRevision: "future" })));
    const response = await app().request(path("/profiles"));
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ kind: "invalidRequest", status: 502 });
  });
  it("forces member visibility and reversible deletion in query parameters", async () => {
    const calls = runtime(url => url.pathname.endsWith("/sessions/s") ? Response.json({ session: { access: { visibility: "universe" } } }) : Response.json({}));
    const api = app("operator");
    expect((await api.request(path("/sessions?visibleTo=mallory"))).status).toBe(200);
    expect(calls.at(-1)!.url.searchParams.get("visibleTo")).toBe("alice");
    expect((await api.request(path("/sessions/s?softDelete=false&sharedOnly=false"), { method: "DELETE" })).status).toBe(200);
    expect(calls.at(-1)!.url.searchParams.get("softDelete")).toBe("true");
    expect(calls.at(-1)!.url.searchParams.get("sharedOnly")).toBe("true");
  });
  it("inspects only session policy bodies, enforcing prepared profiles and retention", async () => {
    let forwarded = "";
    runtime(async (_url, init) => { forwarded = await new Response(init.body).text(); return Response.json({ session: { id: "new" } }); });
    const api = app("contributor");
    const response = await api.request(path("/sessions"), { method: "POST", headers: { "content-type": "application/json" }, body: '{"displayName":"Work","profile":{"kind":"named","profileId":"saved"}}' });
    expect(response.status).toBe(200);
    expect(JSON.parse(forwarded)).toEqual({ displayName: "Work", profile: { kind: "named", profileId: "saved" }, deleteAfterCloseMs: null });
    expect((await api.request(path("/sessions"), { method: "POST", body: '{"profile":{"kind":"inline","profile":{"instructions":"override"}}}' })).status).toBe(403);
  });
  it("rejects path-owned body fields and unsafe integers before rewriting", async () => {
    const calls = runtime(); const api = app("contributor");
    expect((await api.request(path("/sessions/s/runs"), { method: "POST", body: '{"sessionId":"another","source":{"type":"input","items":[]}}' })).status).toBe(400);
    expect((await api.request(path("/sessions"), { method: "POST", body: '{"sessionId":9007199254740993}' })).status).toBe(403);
    expect(calls.every(call => call.url.pathname.endsWith("/connection"))).toBe(true);
  });
  it("bounds policy bodies even without Content-Length", async () => {
    const calls = runtime();
    const response = await app("contributor").request(path("/sessions"), { method: "POST", body: JSON.stringify({ displayName: "x".repeat(1024 * 1024) }) });
    expect(response.status).toBe(413); expect(calls).toHaveLength(1);
  });
  it("forwards binary responses and cancels the upstream reader", async () => {
    const canceled = vi.fn(); let upstreamSignal: AbortSignal | null | undefined;
    runtime((_url, init) => { upstreamSignal = init.signal; return new Response(new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array([0, 255, 1])); }, cancel: canceled }), { headers: { "content-type": "application/octet-stream" } }); });
    const response = await app("viewer").request(path(`/blobs/${encodeURIComponent(`sha256:${"a".repeat(64)}`)}/content`));
    expect(response.status).toBe(200);
    const reader = response.body!.getReader();
    expect((await reader.read()).value).toEqual(new Uint8Array([0, 255, 1]));
    await reader.cancel();
    expect(canceled).toHaveBeenCalledOnce(); expect(upstreamSignal?.aborted).toBe(true);
  });
  it("forwards byte ranges of blob downloads", async () => {
    let upstreamRange: string | null = null;
    runtime((_url, init) => { upstreamRange = new Headers(init.headers).get("range"); return new Response(new Uint8Array([1, 2]), { status: 206, headers: { "content-type": "application/octet-stream", "content-range": "bytes 8-9/10", "accept-ranges": "bytes" } }); });
    const response = await app("viewer").request(path(`/blobs/${encodeURIComponent(`sha256:${"a".repeat(64)}`)}/content`), { headers: { range: "bytes=8-" } });
    expect(upstreamRange).toBe("bytes=8-");
    expect(response.status).toBe(206);
    expect(response.headers.get("content-range")).toBe("bytes 8-9/10");
    expect(response.headers.get("accept-ranges")).toBe("bytes");
    expect(new Uint8Array(await response.arrayBuffer())).toEqual(new Uint8Array([1, 2]));
  });
  it("ends the event stream cleanly when the watch must be reauthorized", async () => {
    vi.useFakeTimers();
    try {
      const event = new TextEncoder().encode("event: watch/reset\ndata: {}\n\n");
      const calls = runtime((_url, { signal }) => new Response(new ReadableStream({
        start(controller) {
          controller.enqueue(event);
          signal!.addEventListener("abort", () => controller.error(signal!.reason), { once: true });
        },
      }), { headers: { "content-type": "text/event-stream" } }));
      const response = await app().request(path("/events"));
      const reader = response.body!.getReader();
      expect((await reader.read()).value).toEqual(event);
      const pending = reader.read();
      const upstream = calls.at(-1)!.init.signal!;
      vi.advanceTimersByTime(59_999);
      expect(upstream.aborted).toBe(false);
      vi.advanceTimersByTime(1);
      expect(upstream.aborted).toBe(true);
      await expect(pending).resolves.toMatchObject({ done: true });
    } finally {
      vi.useRealTimers();
    }
  });
  it("propagates an unexpected upstream watch failure", async () => {
    const failure = new DOMException("Runtime disconnected", "AbortError");
    runtime(() => new Response(new ReadableStream({ start(controller) { controller.error(failure); } }), {
      headers: { "content-type": "text/event-stream" },
    }));
    const response = await app().request(path("/events"));
    await expect(response.text()).rejects.toBe(failure);
  });
  it("uses native lifecycle services instead of forwarding universe deletion", async () => {
    const calls = runtime(); mocks.remove.mockResolvedValue({ state: "deleting" });
    const response = await app("owner", undefined, true).request(path(""), { method: "DELETE" });
    expect(response.status).toBe(200);
    expect(mocks.remove).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ handle: "acme" }));
    expect(calls).toHaveLength(1);
  });
  it.each(["member", "creator", "admin"] as const)("does not let an organization %s who owns a universe allocate deployment providers", async organizationRole => {
    const calls = runtime();
    const api = app("owner", undefined, false, { organizationRole });
    for (const method of ["PUT", "DELETE"]) {
      const response = await api.request(path("/deployment-provider-bindings/compute"), {
        method,
        ...(method === "PUT" ? { body: '{"enabled":true}', headers: { "content-type": "application/json" } } : {}),
      });
      expect(response.status).toBe(403);
      expect(await response.json()).toMatchObject({ kind: "forbidden", detail: "deployment permission required: configureDeployment" });
    }
    expect(mocks.access).not.toHaveBeenCalled();
    expect(calls.every(call => call.url.pathname.endsWith("/connection"))).toBe(true);
  });
  it.each(["PUT", "DELETE"])("allows scoped Platform Admin %s requests to manage a universe's provider bindings", async method => {
    const auth = userSubject("alice", true);
    const operation = method === "PUT" ? "deployment/environment-providers/bindings/put" : "deployment/environment-providers/bindings/delete";
    auth.credential = { kind: "personalKey", id: "admin-key", restrictions: { organizationIds: ["organization"], universeIds: [runtimeUniverse], operations: [operation] } };
    let forwarded = "";
    const calls = runtime(async (_url, init) => {
      forwarded = await new Response(init.body).text();
      return Response.json({ binding: { universeId: runtimeUniverse, providerId: "compute", enabled: true } });
    });
    const response = await app("owner", undefined, true, { auth }).request(path("/deployment-provider-bindings/compute"), {
      method,
      ...(method === "PUT" ? { body: '{"enabled":true}', headers: { "content-type": "application/json" } } : {}),
    });
    expect(response.status).toBe(200);
    expect(calls).toHaveLength(2);
    expect(calls[1]!.url.pathname).toBe(`/api/v1/universes/${runtimeUniverse}/deployment-provider-bindings/compute`);
    expect(calls[1]!.init.method).toBe(method);
    expect(forwarded).toBe(method === "PUT" ? '{"enabled":true}' : "");
    expect(new Headers(calls[1]!.init.headers).get("x-lightspeed-actor")).toBe("alice");
  });
  it.each([
    ["read-only", { readOnly: true }],
    ["another organization", { organizationIds: ["another-organization"] }],
    ["another universe", { universeIds: ["00000000-0000-0000-0000-000000000002"] }],
    ["another operation", { operations: ["environments/provider-bindings/list"] }],
  ] satisfies Array<[string, CredentialRestrictions]>)("honors %s restrictions on a Platform Admin binding key", async (_label, restrictions) => {
    const auth = userSubject("alice", true);
    auth.credential = { kind: "personalKey", id: "admin-key", restrictions };
    const calls = runtime();
    const api = app("owner", undefined, true, { auth });
    for (const method of ["PUT", "DELETE"]) {
      const response = await api.request(path("/deployment-provider-bindings/compute"), {
        method,
        ...(method === "PUT" ? { body: '{"enabled":true}', headers: { "content-type": "application/json" } } : {}),
      });
      expect(response.status).toBe(403);
      expect(await response.json()).toMatchObject({ kind: "forbidden" });
    }
    expect(calls.every(call => call.url.pathname.endsWith("/connection"))).toBe(true);
  });
  it("requires a Platform-owned universe for administrative binding writes", async () => {
    const calls = runtime();
    const api = app("owner", undefined, true);
    mocks.access.mockResolvedValue(null);
    const response = await api.request(path("/deployment-provider-bindings/compute"), { method: "PUT", body: '{"enabled":true}' });
    expect(response.status).toBe(404);
    expect(calls).toHaveLength(1);
  });
  it.each([false, true])("reports live user Platform Admin authority (%s) in discovery", async platformAdmin => {
    runtime();
    const response = await app("owner", undefined, platformAdmin).request("/api/v1/connection");
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ platform: { principal: { kind: "user", id: "alice", platformAdmin } } });
  });
  it("reports service user authority without implying deployment administration", async () => {
    const principal = { kind: "serviceUser", id: "pipeline", organizationId: "organization", platformAdmin: false, serviceUser: { id: "pipeline", name: "Pipeline" } } as Extract<AuthContext["principal"], { kind: "serviceUser" }>;
    const auth: AuthContext = { principal, actorId: "serviceUser:pipeline", credential: { kind: "serviceUserKey", id: "pipeline-key", restrictions: {} } };
    const calls = runtime();
    const api = app("owner", undefined, false, { auth, organizationRole: "admin" });
    const response = await api.request("/api/v1/connection");
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ platform: { principal: { kind: "serviceUser", id: "pipeline", platformAdmin: false } } });
    expect((await api.request(path("/deployment-provider-bindings/compute"), { method: "PUT", body: '{"enabled":true}' })).status).toBe(403);
    expect(calls.every(call => call.url.pathname.endsWith("/connection"))).toBe(true);
  });
  it("does not expose the upstream credential name in Platform discovery", async () => {
    runtime(); const response = await app().request("/api/v1/connection");
    expect(await response.json()).toMatchObject({ runtime: "platform", caller: { single: false }, platform: { credential: { kind: "session", id: "session-alice" } } });
    expect(await (await app().request("/api/v1/connection")).text()).not.toContain("tokenName");
  });
});
