import { decodeHttpRequest, problemResponse } from "../../../test/http-fixture.js";
import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ModelDefaults } from "@lightspeed-ai/sdk";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes, withGateway } from "./gateway.js";
import { LightspeedApiError } from "@lightspeed-ai/sdk";

const auth = vi.hoisted(() => ({ role: "operator" }));
vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async (_ctx, _c, id: string) => ({
  universe: { lightspeedUniverseId: id, gatewayUrl: "https://engine.example/api/v1" },
  handle: "test", role: auth.role, member: { userId: "member", role: auth.role },
})) }));
beforeEach(() => { auth.role = "operator"; });
afterEach(() => vi.unstubAllGlobals());

function fixture() {
  const requests: { method: string; params: Record<string, unknown> }[] = [];
  let defaults: ModelDefaults = { revision: 2, agentRun: null, speechToText: null };
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(decodeHttpRequest(_url as string | Request | URL, init).universe).toBe("universe");
    expect(new Headers(init.headers).has("x-lightspeed-universe")).toBe(false);
    expect(new Headers(init.headers).get("x-lightspeed-actor")).toBe("member");
    const request = decodeHttpRequest(_url as string | Request | URL, init);
    requests.push({ method: request.method, params: request.params });
    if (request.method === "models/defaults/put") {
      if (request.params.expectedRevision !== defaults.revision) return problemResponse({ type: "about:blank", title: "defaults changed", detail: "defaults changed", status: 409, kind: "conflict" });
      defaults = { ...defaults, [request.params.slot]: request.params.model, revision: defaults.revision + 1 };
    }
    return Response.json({ defaults });
  });
  vi.stubGlobal("fetch", fetch);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: "member" } } as ApiVariables["session"]); await next(); });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/api/v1", lightspeedApiToken: "lsr_fixture" } } as AppContext));
  const call = (method = "GET", body?: unknown) => app.request("/universe/models/defaults", {
    method, headers: { "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return { app, call, requests, fetch };
}

it("reads and writes defaults through the member-scoped runtime client", async () => {
  const f = fixture();
  expect(await (await f.call()).json()).toMatchObject({ revision: 2, agentRun: null });
  const model = { providerId: "private", apiKind: "openai:completions", model: "unlisted" };
  expect(await (await f.call("PUT", { slot: "agentRun", model, expectedRevision: 2 })).json()).toMatchObject({ revision: 3, agentRun: model });
  expect((await f.call("PUT", { slot: "agentRun", model: null, expectedRevision: 2 })).status).toBe(409);
  expect(await (await f.call("PUT", { slot: "agentRun", model: null, expectedRevision: 3 })).json()).toMatchObject({ revision: 4, agentRun: null });
  expect(f.requests.map((request) => request.method)).toEqual(["models/defaults/read", "models/defaults/put", "models/defaults/put", "models/defaults/put"]);
});

it.each(["viewer", "contributor"])("allows %s to inspect but not configure defaults", async (role) => {
  auth.role = role;
  const f = fixture();
  expect((await f.call()).status).toBe(200);
  expect((await f.call("PUT", { slot: "agentRun", model: null, expectedRevision: 2 })).status).toBe(403);
  expect(f.fetch).toHaveBeenCalledTimes(1);
});

it.each([
  { slot: "agentRun", expectedRevision: 2 },
  { slot: "agentRun", model: null },
  { slot: "agentRun", model: { providerId: "openai", apiKind: "openai:audio-transcriptions", model: "speech" }, expectedRevision: 2 },
])("rejects incomplete or incompatible writes before sending them upstream", async (body) => {
  const f = fixture();
  expect((await f.call("PUT", body)).status).toBe(400);
  expect(f.fetch).not.toHaveBeenCalled();
});

it("preserves the typed missing-default error for web clients", async () => {
  const app = new Hono();
  app.get("/", (c) => withGateway(c, async () => { throw new LightspeedApiError({ type: "about:blank", title: "Choose a model", detail: "Choose a model", status: 409, kind: "modelDefaultUnset", modelDefaultSlot: "agentRun" }); }));
  const response = await app.request("/");
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({ kind: "modelDefaultUnset", modelDefaultSlot: "agentRun" });
});

it("tells web clients a held request is pending and which id to retry with", async () => {
  const app = new Hono();
  app.get("/", (c) => withGateway(c, async () => { throw new LightspeedApiError({ type: "about:blank", title: "still held", detail: "still held", status: 409, kind: "operationPending", submissionId: "submission" }); }));
  const response = await app.request("/");
  expect(response.status).toBe(409);
  expect(await response.json()).toMatchObject({ kind: "operationPending", submissionId: "submission" });
});
