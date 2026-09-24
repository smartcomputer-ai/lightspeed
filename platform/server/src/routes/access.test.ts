import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { EffectiveAccess } from "@lightspeed-ai/agent-client";
import type { ApiVariables, AppContext } from "../context.js";
import { requestIdentity } from "../runtime-client.js";
import { accessRoutes } from "./access.js";

vi.mock("./universes.js", () => ({
  universeForSession: vi.fn(async () => ({
    universe: {
      lightspeedUniverseId: "universe",
      gatewayUrl: "https://engine.example/rpc",
    },
    role: "contributor",
  })),
}));
afterEach(() => vi.unstubAllGlobals());
const actor = "11111111-1111-4111-8111-111111111111";
function app() {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (_c, next) =>
    requestIdentity.run({ principal: { id: actor } } as EffectiveAccess, next),
  );
  app.route(
    "/",
    accessRoutes({
      env: {
        lightspeedApiUrl: "https://engine.example/rpc",
        lightspeedApiKey: "lsk_fixture",
      },
    } as AppContext),
  );
  return app;
}
it.each([
  [
    "GET",
    "/universe/access/subjects?q=Alice",
    undefined,
    "access/subjects",
    { query: "Alice" },
  ],
  ["GET", "/universe/access/execution", undefined, "access/execution/read", {}],
  [
    "POST",
    "/universe/access/policy/read",
    { resource: { kind: "session", id: "child" } },
    "access/policy/read",
    { resource: { kind: "session", id: "child" } },
  ],
  [
    "PUT",
    "/universe/access/policy",
    {
      resource: { kind: "bot", id: "assistant" },
      visibility: "restricted",
      grants: [],
      expectedRevision: 7,
    },
    "access/policy/put",
    {
      resource: { kind: "bot", id: "assistant" },
      visibility: "restricted",
      grants: [],
      expectedRevision: 7,
    },
  ],
  [
    "PUT",
    "/universe/access/policy",
    {
      resource: { kind: "environment", id: "production" },
      visibility: "restricted",
      grants: [
        { subject: { kind: "principal", id: actor }, permission: "use" },
      ],
      expectedRevision: 2,
    },
    "access/policy/put",
    {
      resource: { kind: "environment", id: "production" },
      visibility: "restricted",
      grants: [
        { subject: { kind: "principal", id: actor }, permission: "use" },
      ],
      expectedRevision: 2,
    },
  ],
] as const)(
  "forwards %s %s with current user authority",
  async (method, path, body, rpcMethod, params) => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_url: unknown, init: RequestInit) => {
        expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe(
          `user:${actor}`,
        );
        expect(new Headers(init.headers).get("x-lightspeed-universe")).toBe(
          "universe",
        );
        const rpc = JSON.parse(String(init.body));
        expect(rpc.method).toBe(rpcMethod);
        expect(rpc.params).toEqual(params);
        return Response.json({
          id: rpc.id,
          result: { result: {}, notifications: [] },
        });
      }),
    );
    const response = await app().request(path, {
      method,
      ...(body
        ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          }
        : {}),
    });
    expect(response.status).toBe(200);
  },
);
it.each([
  {
    resource: { kind: "session", id: "s" },
    visibility: "restricted",
    grants: [],
  },
  {
    resource: { kind: "session", id: "s" },
    visibility: "restricted",
    grants: [],
    expectedRevision: 1,
    principalId: actor,
  },
])(
  "requires a revision and forbids actor overrides when sharing",
  async (body) => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    expect(
      (
        await app().request("/universe/access/policy", {
          method: "PUT",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        })
      ).status,
    ).toBe(400);
    expect(fetch).not.toHaveBeenCalled();
  },
);
