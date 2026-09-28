import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ModelDefaults } from "@lightspeed-ai/agent-client";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes, withGateway } from "./gateway.js";
import { LightspeedRpcError } from "@lightspeed-ai/agent-client";

const auth = vi.hoisted(() => ({ role: "operator" }));
vi.mock("./universes.js", () => ({ universeForSession: vi.fn(async (_ctx, _c, id: string) => ({
  universe: { lightspeedUniverseId: id, gatewayUrl: "https://engine.example/rpc" },
  slug: "test", role: auth.role, member: { userId: "member", role: auth.role },
})) }));
beforeEach(() => { auth.role = "operator"; });
afterEach(() => vi.unstubAllGlobals());

function fixture() {
  const requests: { method: string; params: Record<string, unknown> }[] = [];
  let defaults: ModelDefaults = { revision: 2, agentRun: null, speechToText: null };
  const fetch = vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(new Headers(init.headers).get("x-lightspeed-universe")).toBe("universe");
    expect(new Headers(init.headers).get("x-lightspeed-actor")).toBe("member");
    const rpc = JSON.parse(String(init.body));
    requests.push(rpc);
    if (rpc.method === "models/defaults/put") {
      if (rpc.params.expectedRevision !== defaults.revision) return Response.json({ id: rpc.id,
        error: { code: -32009, message: "defaults changed", data: { kind: "conflict", message: "defaults changed" } } });
      defaults = { ...defaults, [rpc.params.slot]: rpc.params.model, revision: defaults.revision + 1 };
    }
    return Response.json({ id: rpc.id, result: { result: { defaults }, notifications: [] } });
  });
  vi.stubGlobal("fetch", fetch);
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("session", { user: { id: "member" } } as ApiVariables["session"]); await next(); });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext));
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
  app.get("/", (c) => withGateway(c, async () => { throw new LightspeedRpcError({ code: -32014, message: "Choose a model", data: { kind: "model_default_unset", message: "Choose a model", modelDefaultSlot: "agentRun" } }); }));
  const response = await app.request("/");
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({ kind: "model_default_unset", modelDefaultSlot: "agentRun" });
});
