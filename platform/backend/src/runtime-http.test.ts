import { userSubject } from "../test/subject-fixture.js";
import { Hono } from "hono";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HTTP_OPERATIONS } from "@lightspeed-ai/sdk/server";
import type { AppContext, ApiVariables } from "./context.js";
import { runtimeHttpRoutes } from "./runtime-http.js";
import { ADMIN_OPERATIONS, DENIED_OPERATIONS, MEMBER_OPERATIONS, NATIVE_OPERATIONS } from "./runtime-admission.js";

const mocks = vi.hoisted(() => ({ access: vi.fn(), create: vi.fn(), remove: vi.fn(), handle: vi.fn() }));
vi.mock("./routes/universes.js", () => ({ universeForRuntimeReference: mocks.access, listUniverseAccesses: vi.fn(async () => []), deleteManagedUniverse: mocks.remove, renameManagedUniverse: vi.fn(), updateUniverseLifecycle: vi.fn() }));
vi.mock("./universe-handles.js", () => ({ requireAvailableCachedHandle: mocks.handle }));
const runtimeUniverse = "00000000-0000-0000-0000-000000000001";
const token = "platform-test-token-12345678901234567890";
const connection = { runtime: "runtime", contractRevision: "1", caller: { single: false, tokenName: "platform" }, serverInfo: { name: "Lightspeed", version: "1" } };
const access = (role = "owner") => ({ universe: { id: "platform-id", lightspeedUniverseId: runtimeUniverse, gatewayUrl: null, features: {} }, handle: "acme", role, member: { userId: "alice", role } });

function app(role = "owner", endpoint = "https://runtime.test/api/v1", platformAdmin = false) {
  const auth = userSubject("alice", platformAdmin);
  mocks.access.mockResolvedValue({ ...access(role), auth, accountRole: platformAdmin ? "admin" : "member", account: { id: "account", requireProtected: false } });
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
    expect(await response.json()).toMatchObject({ status: 403, kind: "forbidden", detail: "operator role required" });
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
  it("uses native lifecycle services instead of forwarding universe deletion", async () => {
    const calls = runtime(); mocks.remove.mockResolvedValue({ state: "deleting" });
    const response = await app("owner", undefined, true).request(path(""), { method: "DELETE" });
    expect(response.status).toBe(200);
    expect(mocks.remove).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ handle: "acme" }), "alice");
    expect(calls).toHaveLength(1);
  });
  it("does not expose the upstream credential name in Platform discovery", async () => {
    runtime(); const response = await app().request("/api/v1/connection");
    expect(await response.json()).toMatchObject({ runtime: "platform", caller: { single: false }, platform: { credential: { kind: "session", id: "session-alice" } } });
    expect(await (await app().request("/api/v1/connection")).text()).not.toContain("tokenName");
  });
});
