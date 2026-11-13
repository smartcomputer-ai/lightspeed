import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { botRoutes } from "./bots.js";
import { channelUniverseRoutes } from "./channel-accounts.js";

afterEach(() => vi.unstubAllGlobals());

/// The bot and channel routes of one universe that stored `features`, for an
/// admin of it. Every outbound request (the runtime, Telegram, a connector's
/// health endpoint) is recorded and answered as an empty success.
function setup(features: Record<string, boolean>) {
  const universe = {
    id: "platform-universe",
    organizationId: "org",
    lightspeedUniverseId: "33333333-3333-4333-8333-333333333333",
    gatewayUrl: null,
    features,
  };
  const requests: { url: string; method?: string }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (input: unknown, init?: RequestInit) => {
    const url = String(input);
    if (!url.startsWith("https://core.example/")) {
      requests.push({ url });
      return Response.json({});
    }
    const rpc = JSON.parse(String(init?.body));
    requests.push({ url, method: rpc.method });
    return Response.json({ id: rpc.id, result: { result: {}, notifications: [] } });
  }));
  // Each request reads the universe, then the caller's membership.
  const reads = () => {
    const queue: unknown[][] = [[{ universe, handle: "test" }], [{ role: "admin" }]];
    const chain: Record<string, unknown> = {};
    for (const step of ["from", "innerJoin", "where"]) chain[step] = () => chain;
    chain.limit = async () => queue.shift() ?? [];
    return chain;
  };
  let chain = reads();
  const ctx = {
    db: { select: () => chain },
    env: {
      lightspeedApiUrl: "https://core.example/rpc",
      lightspeedApiKey: "lsk_fixture",
      channelsHealthUrls: ["https://connector.example/"],
    },
  } as unknown as AppContext;
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    chain = reads();
    c.set("session", { user: { id: "caller" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", botRoutes(ctx));
  app.route("/", channelUniverseRoutes(ctx));
  const request = (method: string, path: string, body?: unknown) => app.request(`/platform-universe${path}`, {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, requests };
}

const botsOff = { error: "Bots are turned off in this universe" };
const channelsOff = { error: "Channels are turned off in this universe" };

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

it.each([
  ["GET", "/channel-accounts", undefined],
  ["GET", "/channel-pairings", undefined],
  ["GET", "/channel-status", undefined],
  ["POST", "/channel-accounts", { account: { accountId: "a" } }],
  ["POST", "/channel-accounts/connect", { provider: "whatsapp", phoneNumber: "+15550100" }],
  ["POST", "/channel-accounts/connect", { provider: "telegram", token: "123:telegram-token" }],
])("refuses %s %s while Channels are off, before any outbound request", async (method, path, body) => {
  // Bots alone do not open channels.
  const { request, requests } = setup({ bots: true });
  const response = await request(method, path, body);
  expect(response.status).toBe(403);
  expect(await response.json()).toEqual(channelsOff);
  // Neither the runtime, nor Telegram with the token, nor a connector.
  expect(requests).toEqual([]);
});

it("serves bots with Bots on while channels stay refused", async () => {
  const { request, requests } = setup({ bots: true });
  expect((await request("GET", "/bots")).status).toBe(200);
  expect((await request("GET", "/channel-accounts")).status).toBe(403);
  expect(requests.map((entry) => entry.method)).toEqual(["bots/list"]);
});

it("serves channels once both switches are on", async () => {
  const { request, requests } = setup({ bots: true, channels: true });
  expect((await request("GET", "/channel-accounts")).status).toBe(200);
  expect((await request("GET", "/channel-status")).status).toBe(200);
  expect(requests).toEqual([
    { url: "https://core.example/rpc", method: "channels/accounts/list" },
    { url: "https://connector.example/healthz" },
  ]);
});
