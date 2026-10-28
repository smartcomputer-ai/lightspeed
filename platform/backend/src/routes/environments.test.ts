import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { environmentDeploymentRoutes } from "./environment-deployment.js";
import { gatewayRoutes } from "./gateway.js";

vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: { lightspeedUniverseId: "universe", gatewayUrl: "https://engine.example/rpc" },
    role: "operator",
    member: { userId: "user", role: "operator" },
  })),
}));

type Call = { method: string; params: Record<string, unknown> };
let calls: Call[];
/// Providers whose `environments/templates/list` call the engine refuses.
let failingProviders: Set<string>;

function template(providerId: string) {
  return { providerId, templateId: `${providerId}-small`, displayName: `${providerId} small` };
}

function engineResult(method: string, params: Record<string, unknown>): unknown {
  switch (method) {
    case "environments/provider-bindings/list":
      return {
        bindings: [
          { providerId: "alpha", enabled: true, createdAtMs: 1, updatedAtMs: 1 },
          { providerId: "beta", enabled: true, createdAtMs: 1, updatedAtMs: 1 },
          { providerId: "gamma", enabled: false, createdAtMs: 1, updatedAtMs: 1 },
        ],
      };
    case "environments/templates/list":
      return { templates: [template(String(params.providerId))] };
    case "environments/list":
      return { environments: [{ environmentId: "env_1" }], nextAfter: "env_1" };
    case "environments/update":
      return { environment: { environmentId: params.environmentId } };
    case "environments/registration-keys/update":
      return { registrationKey: { registrationKeyId: params.registrationKeyId } };
    case "environments/registration-keys/rotate":
      return { registrationKey: { registrationKeyId: params.registrationKeyId }, secret: "lsr_secret" };
    case "deployment/environment-providers/put":
      return { provider: { providerId: params.providerId } };
    case "deployment/environment-providers/bindings/put":
    case "deployment/environment-providers/bindings/delete":
      return { binding: { providerId: params.providerId, enabled: params.enabled ?? false } };
    default:
      throw new Error(`unexpected engine call ${method}`);
  }
}

beforeEach(() => {
  calls = [];
  failingProviders = new Set();
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    const params = (rpc.params ?? {}) as Record<string, unknown>;
    calls.push({ method: rpc.method, params });
    if (rpc.method === "environments/templates/list" && failingProviders.has(String(params.providerId))) {
      return Response.json({
        id: rpc.id,
        error: { code: -32603, message: "provider unreachable", data: { kind: "internal" } },
      });
    }
    return Response.json({ id: rpc.id, result: { result: engineResult(rpc.method, params), notifications: [] } });
  }));
});
afterEach(() => vi.unstubAllGlobals());

const ctx = {
  env: { lightspeedApiUrl: "https://engine.example/rpc", lightspeedApiKey: "lsk_fixture" },
} as unknown as AppContext;

function app(platformRole?: string) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "user", role: platformRole } } as ApiVariables["session"]);
    await next();
  });
  app.route("/universes", gatewayRoutes(ctx));
  app.route("/admin", environmentDeploymentRoutes(ctx));
  return app;
}

function request(method: string, path: string, body?: unknown, platformRole?: string) {
  return app(platformRole).request(path, {
    method,
    headers: { "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}

const callsOf = (method: string) => calls.filter((call) => call.method === method);

it("lists templates per enabled provider and drops a provider that fails", async () => {
  failingProviders.add("beta");
  const response = await request("GET", "/universes/u/environment-templates");
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual([template("alpha")]);
  expect(callsOf("environments/templates/list").map((call) => call.params)).toEqual([
    { providerId: "alpha" },
    { providerId: "beta" },
  ]);
});

it("lists one provider's templates directly when asked", async () => {
  const response = await request("GET", "/universes/u/environment-templates?providerId=gamma");
  expect(await response.json()).toEqual([template("gamma")]);
  expect(callsOf("environments/provider-bindings/list")).toEqual([]);
});

it("maps the environment list query onto one environments/list page", async () => {
  const response = await request(
    "GET",
    "/universes/u/environments?includeClosed=true&limit=50&after=env_0&state=open&providerId=alpha",
  );
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ environments: [{ environmentId: "env_1" }], nextAfter: "env_1" });
  expect(callsOf("environments/list")).toEqual([{
    method: "environments/list",
    params: { includeClosed: true, limit: 50, after: "env_0", state: "open", providerId: "alpha" },
  }]);
});

it("leaves includeClosed unset unless it is true", async () => {
  await request("GET", "/universes/u/environments?includeClosed=false");
  expect(callsOf("environments/list")[0]?.params).toEqual({});
});

it.each([
  ["limit=0"],
  ["limit=501"],
  ["limit=two"],
  ["state=asleep"],
  ["includeClosed=yes"],
])("rejects the list query %s before calling the engine", async (query) => {
  const response = await request("GET", `/universes/u/environments?${query}`);
  expect(response.status).toBe(400);
  expect(callsOf("environments/list")).toEqual([]);
});

it("clears an environment's display name when the update sends null", async () => {
  const response = await request("PATCH", "/universes/u/environments/env_1", { displayName: null });
  expect(response.status).toBe(200);
  expect(callsOf("environments/update")).toEqual([{
    method: "environments/update",
    params: { environmentId: "env_1", metadata: {} },
  }]);
});

it("replaces an environment's display name and metadata", async () => {
  await request("PATCH", "/universes/u/environments/env_1", { displayName: "build box", metadata: { team: "ci" } });
  expect(callsOf("environments/update")[0]?.params).toEqual({
    environmentId: "env_1",
    displayName: "build box",
    metadata: { team: "ci" },
  });
});

it("passes a registration key update through as the complete policy", async () => {
  const response = await request("PUT", "/universes/u/environment-registration-keys/rk_1", {
    displayName: "CI runners",
    maxActiveEnvironments: 4,
    offlineCloseAfterMs: 600_000,
  });
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual({ registrationKeyId: "rk_1" });
  expect(callsOf("environments/registration-keys/update")[0]?.params).toEqual({
    registrationKeyId: "rk_1",
    displayName: "CI runners",
    maxActiveEnvironments: 4,
    offlineCloseAfterMs: 600_000,
  });
});

it("rejects a registration key update without a name", async () => {
  const response = await request("PUT", "/universes/u/environment-registration-keys/rk_1", {});
  expect(response.status).toBe(400);
  expect(callsOf("environments/registration-keys/update")).toEqual([]);
});

it("returns a rotated registration key's secret", async () => {
  const response = await request("POST", "/universes/u/environment-registration-keys/rk_1/rotate");
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ secret: "lsr_secret" });
  expect(callsOf("environments/registration-keys/rotate")[0]?.params).toEqual({ registrationKeyId: "rk_1" });
});

it("puts and deletes a universe binding by provider id", async () => {
  const put = await request("PUT", "/admin/universes/u1/environment-provider-bindings/alpha", { enabled: false }, "admin");
  expect(put.status).toBe(200);
  const deleted = await request("DELETE", "/admin/universes/u1/environment-provider-bindings/alpha", undefined, "admin");
  expect(deleted.status).toBe(200);
  expect(callsOf("deployment/environment-providers/bindings/put")[0]?.params).toEqual({
    universeId: "u1",
    providerId: "alpha",
    enabled: false,
  });
  expect(callsOf("deployment/environment-providers/bindings/delete")[0]?.params).toEqual({
    universeId: "u1",
    providerId: "alpha",
  });
});

it("puts a provider without a credential", async () => {
  const response = await request("PUT", "/admin/environment-providers/alpha", {
    controllerEndpoint: "unix:///run/incus-controller.sock",
    dataRouteBase: "wss://incus.example/routes",
  }, "admin");
  expect(response.status).toBe(200);
  expect(callsOf("deployment/environment-providers/put")[0]?.params).toEqual({
    providerId: "alpha",
    controllerEndpoint: "unix:///run/incus-controller.sock",
    dataRouteBase: "wss://incus.example/routes",
  });
});

it("rejects a provider put with both a credential and clearCredential", async () => {
  const response = await request("PUT", "/admin/environment-providers/alpha", {
    controllerEndpoint: "wss://incus.example/controller",
    credential: "secret",
    clearCredential: true,
  }, "admin");
  expect(response.status).toBe(400);
  expect(callsOf("deployment/environment-providers/put")).toEqual([]);
});

it("keeps the deployment routes for platform admins", async () => {
  const response = await request("PUT", "/admin/universes/u1/environment-provider-bindings/alpha", { enabled: true });
  expect(response.status).toBe(403);
  expect(calls).toEqual([]);
});
