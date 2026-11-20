import { userSubject } from "../../test/subject-fixture.js";
import { decodeHttpRequest } from "../../../test/http-fixture.js";
import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { botRoutes } from "./bots.js";

afterEach(() => vi.unstubAllGlobals());

/// The bot routes of one universe, with runtime requests recorded.
function setup(features: Record<string, boolean>) {
  const universe = {
    id: "platform-universe",
    organizationId: "org",
    lightspeedUniverseId: "33333333-3333-4333-8333-333333333333",

    features, handle: "test", status: "active", runtimeConfiguration: {}, accessPolicy: {},
  };
  const requests: { url: string; method?: string }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    if (!url.startsWith("https://core.example/")) {
      requests.push({ url });
      return Response.json({});
    }
    const request = decodeHttpRequest(input as string | Request | URL, init);
    requests.push({ url, method: request.method });
    return Response.json({});
  }));
  // Each request reads the universe, then the caller's membership.
  const reads = () => {
    const queue: unknown[][] = [[universe], [{ id: "org", enabled: true, membershipPolicy: {} }], [{ role: "member" }], [{ role: "owner" }]];
    const chain: Record<string, unknown> = {};
    for (const step of ["from", "innerJoin", "where"]) chain[step] = () => chain;
    chain.limit = async () => queue.shift() ?? [];
    return chain;
  };
  let chain = reads();
  const ctx = {
    db: { select: () => chain },
    env: {
      lightspeedApiUrl: "https://core.example/api/v1",
      lightspeedApiToken: "lsr_fixture",
    },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    chain = reads();
    c.set("session", { user: { id: "caller" } } as ApiVariables["session"]);
    c.set("auth", userSubject(c.get("session").user.id, c.get("session").user.role === "admin"));
    await next();
  });
  app.route("/", botRoutes(ctx));
  const request = (method: string, path: string, body?: unknown) => app.request(`/platform-universe${path}`, {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, requests };
}

const botsOff = { error: "Bots are turned off in this universe" };

it.each([
  ["GET", "/bots", undefined],
  ["POST", "/bots", { bot: { botId: "helper" } }],
  ["GET", "/bots/helper", undefined],
  ["GET", "/bots/helper/triggers", undefined],
  ["DELETE", "/bots/helper", undefined],
])("refuses %s %s for an admin while Bots are off, without reaching the runtime", async (method, path, body) => {
  const { request, requests } = setup({});
  const response = await request(method, path, body);
  expect(response.status).toBe(403);
  expect(await response.json()).toEqual(botsOff);
  expect(requests).toEqual([]);
});

it("serves bots with Bots on", async () => {
  const { request, requests } = setup({ bots: true });
  expect((await request("GET", "/bots")).status).toBe(200);
  expect(requests.map((entry) => entry.method)).toEqual(["bots/list"]);
});
