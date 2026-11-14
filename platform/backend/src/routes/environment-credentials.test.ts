import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { gatewayRoutes } from "./gateway.js";

vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: { lightspeedUniverseId: "universe", gatewayUrl: "https://engine.example/rpc" },
    role: "admin",
    member: { userId: "user", role: "admin" },
  })),
}));

/// Every request the routes send to the runtime, answered with one grant, one
/// binding and no existing bindings.
let requests: Array<{ method: string; params: Record<string, unknown> }>;
beforeEach(() => {
  requests = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    requests.push(rpc);
    const result = {
      grant: { grantId: "authgrant_new", status: "active" },
      credential: { environmentId: "build-box", envName: rpc.params?.envName },
      credentials: [],
    };
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  }));
});
afterEach(() => vi.unstubAllGlobals());

function post(path: string, body: unknown) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "user" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", gatewayRoutes({ env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" } } as AppContext));
  return app.request(`/universe${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function sent(method: string) {
  return requests.filter((request) => request.method === method).map((request) => request.params);
}

// These credentials exist to be handed to the processes of a machine, which
// the runtime allows only for a retrievable credential.
it("creates an environment secret as a retrievable credential", async () => {
  const response = await post("/secrets/environment", { displayName: "Deploy key", value: "v" });
  expect(response.status).toBe(201);
  expect(sent("auth/grants/import")).toEqual([expect.objectContaining({ exposure: "retrievable" })]);
});

it("imports a coding-agent subscription as a retrievable credential", async () => {
  const response = await post("/integrations/subscriptions", { provider: "anthropic", credential: "sk-ant-oat01-abc" });
  expect(response.status).toBe(201);
  expect(sent("auth/grants/import")).toEqual([expect.objectContaining({ providerId: "anthropic", exposure: "retrievable" })]);
});

it("grants a GitHub App installation as a retrievable credential", async () => {
  const response = await post("/integrations/github/apps/github/installations/42/grant", {});
  expect(response.status).toBe(201);
  expect(sent("auth/github/installations/grant")).toEqual([
    expect.objectContaining({ providerId: "github", installationId: 42, exposure: "retrievable" }),
  ]);
});

it("leaves a pasted token brokered unless it is created retrievable", async () => {
  expect((await post("/secrets/grants", { token: "t" })).status).toBe(201);
  expect((await post("/secrets/grants", { token: "t", exposure: "retrievable" })).status).toBe(201);
  expect(sent("auth/grants/import").map((params) => params.exposure)).toEqual(["brokered", "retrievable"]);
});

it("binds a credential or a provider key, and nothing that names a stored secret", async () => {
  for (const source of [
    { type: "authGrant", grantId: "authgrant_new" },
    { type: "authProviderCredential", providerId: "model:openai" },
  ]) {
    const response = await post("/environments/build-box/credentials", { envName: "TOKEN", source });
    expect(response.status).toBe(201);
  }
  expect(sent("environments/credentials/bind").map((params) => params.source)).toEqual([
    { type: "authGrant", grantId: "authgrant_new" },
    { type: "authProviderCredential", providerId: "model:openai" },
  ]);

  requests = [];
  const refused = await post("/environments/build-box/credentials", {
    envName: "TOKEN",
    source: { type: "directSecret", secretId: "authsec_1" },
  });
  expect(refused.status).toBe(400);
  expect(requests).toEqual([]);
});
