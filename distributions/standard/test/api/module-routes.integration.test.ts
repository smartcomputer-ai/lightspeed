import { eq } from "drizzle-orm";
import { afterAll, afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed-ai/platform-db";
import { LightspeedClient } from "@lightspeed-ai/sdk";
import { HTTP_CONTRACT_REVISION } from "@lightspeed-ai/sdk/server";
import { buildStandardApi } from "../../api.js";
import { decodeHttpRequest, problemResponse } from "../../../../platform/test/http-fixture.js";
import { organizationFixture } from "../../../../platform/backend/test/organization-fixture.js";
import { responseCookies } from "../../../../platform/backend/test/identity-fixture.js";
import { buildApp } from "../../../../platform/backend/src/api.js";

let fixture: Awaited<ReturnType<typeof organizationFixture>>;
let app: ReturnType<typeof buildStandardApi>;
let cookie: string;
let actor: string;
const base = "/api/ui/universes/work";
const runtimeCalls: ReturnType<typeof decodeHttpRequest>[] = [];

beforeAll(async () => {
  fixture = await organizationFixture();
  app = buildStandardApi(fixture.context);
  const login = await app.request("/api/auth/sign-in/email", {
    method: "POST", headers: { "content-type": "application/json", origin: fixture.env.baseUrl },
    body: JSON.stringify({ email: fixture.env.adminEmail, password: fixture.env.adminPassword }),
  });
  expect(login.status).toBe(200);
  cookie = responseCookies(login);
  actor = ((await login.json()) as { user: { id: string } }).user.id;
}, 30_000);
afterAll(async () => { await fixture?.postgres.close(); });
afterEach(() => vi.unstubAllGlobals());

beforeEach(async () => {
  await fixture.db.delete(schema.organizationModules);
  await fixture.db.update(schema.user).set({ role: "admin" }).where(eq(schema.user.id, actor));
  await fixture.db.update(schema.universes).set({ features: { bots: true } }).where(eq(schema.universes.id, fixture.universe.id));
  runtimeCalls.length = 0;
  const sessions = [{ id: "conversation", displayName: "Conversation", access: { visibility: "universe" } }];
  vi.stubGlobal("fetch", async (input: Request | URL | string, init?: RequestInit) => {
    const body = init?.body instanceof ReadableStream ? await new Response(init.body).text() : init?.body;
    const call = decodeHttpRequest(input, { ...init, body });
    runtimeCalls.push(call);
    switch (call.method) {
      case "initialize": return Response.json({ contractRevision: HTTP_CONTRACT_REVISION, runtime: "runtime" });
      case "bots/list": return Response.json({ bots: [{ botId: "planner" }] });
      case "session/start": {
        const session = { id: "started-session", displayName: call.params.displayName, access: { visibility: "restricted" } };
        sessions.push(session);
        return Response.json({ session });
      }
      case "session/list": return Response.json({ sessions });
      case "session/read": {
        const session = sessions.find(({ id }) => id === call.params.sessionId);
        return session ? Response.json({ session }) : problemResponse({ status: 404, kind: "notFound", detail: "session not found" });
      }
      default: throw new Error(`Unexpected upstream operation ${call.method}`);
    }
  });
});

function request(path: string, method = "GET", body?: unknown, target = app) {
  return target.request(path, {
    method, headers: { cookie, origin: fixture.env.baseUrl, "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}
async function setEnabled(id: string, enabled: boolean) {
  const result = await request(`/api/v1/platform/organizations/organization-a/modules/${id}`, "PUT", { enabled });
  expect(result.status).toBe(200);
}

it.each(["disabled", "omitted"] as const)("keeps the core session SDK available when Session Browser is %s", async state => {
  if (state === "disabled") await setEnabled("sessions", false);
  const target = state === "disabled" ? app : buildApp(fixture.context);
  expect((await request(`${base}/modules/sessions`, "GET", undefined, target)).status).toBe(404);
  expect((await request(`${base}/modules/sessions`, "POST", {}, target)).status).toBe(404);
  expect(runtimeCalls).toHaveLength(0);

  const client = new LightspeedClient({
    endpoint: fixture.env.baseUrl,
    universe: "work",
    headers: { cookie, origin: fixture.env.baseUrl },
    fetch: async (input, init) => target.request(input, init),
  });
  const { session } = await client.startSession({ displayName: "Started through the SDK" });
  expect(session.id).toBe("started-session");
  expect((await client.listSessions()).sessions).toEqual(expect.arrayContaining([expect.objectContaining({ id: session.id })]));
  expect((await client.readSession(session.id)).session).toMatchObject({ id: session.id, displayName: "Started through the SDK" });
  const calls = runtimeCalls.filter(call => call.method !== "initialize");
  expect(calls.map(call => call.method)).toEqual(["session/start", "session/list", "session/read"]);
  for (const call of calls) {
    expect(call.universe).toBe(fixture.universe.lightspeedUniverseId);
    expect(call.headers.get("x-lightspeed-actor")).toBe(actor);
  }
});

it("mounts each installed browser API and keeps shared conversations independent", async () => {
  expect(await (await request(`${base}/modules/sessions`)).json()).toMatchObject({ sessions: [{ id: "conversation" }] });
  expect(await (await request(`${base}/modules/bots`)).json()).toEqual({ bots: [{ botId: "planner" }] });
  expect(runtimeCalls.map(call => [call.universe, call.headers.get("x-lightspeed-actor")])).toEqual([
    [fixture.universe.lightspeedUniverseId, actor], [fixture.universe.lightspeedUniverseId, actor],
  ]);

  await setEnabled("sessions", false);
  expect((await request(`${base}/sessions/conversation`)).status).toBe(200);
  expect((await request(`${base}/modules/bots`)).status).toBe(200);

  await setEnabled("sessions", true);
  await setEnabled("bots", false);
  expect((await request(`${base}/modules/bots/planner/triggers`)).status).toBe(404);
  expect((await request(`${base}/modules/sessions`)).status).toBe(200);
  expect((await request("/api/v1/universes/work/bots")).status).toBe(200);

  const bareHost = buildApp(fixture.context);
  for (const id of ["sessions", "bots"]) expect((await request(`${base}/modules/${id}`, "GET", undefined, bareHost)).status).toBe(404);
  expect((await request(`${base}/sessions/conversation`, "GET", undefined, bareHost)).status).toBe(200);
});

it("checks browser admission, the universe switch, and the action on the actual bot operation", async () => {
  expect((await app.request(`${base}/modules/bots`)).status).toBe(401);
  expect((await app.request(`${base}/modules/bots`, { method: "POST", headers: { cookie, origin: "https://outside.example" } })).status).toBe(403);
  await fixture.db.update(schema.universes).set({ features: { bots: false } }).where(eq(schema.universes.id, fixture.universe.id));
  for (const [method, suffix] of [["GET", ""], ["POST", ""], ["GET", "/planner"], ["GET", "/planner/triggers"], ["DELETE", "/planner"]]) {
    const response = await request(`${base}/modules/bots${suffix}`, method);
    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ error: "Bots are turned off in this universe" });
  }
  await fixture.db.update(schema.universes).set({ features: { bots: true } }).where(eq(schema.universes.id, fixture.universe.id));
  await fixture.db.update(schema.user).set({ role: "user" }).where(eq(schema.user.id, actor));
  expect((await request(`${base}/modules/bots`)).status).toBe(404);
  await fixture.db.insert(schema.member).values({ id: "module-viewer", organizationId: "organization-a", userId: actor, role: "member", createdAt: new Date() });
  await fixture.db.insert(schema.universeGrants).values({ universeId: fixture.universe.id, userId: actor, role: "viewer" });
  for (const [method, suffix] of [["DELETE", "/planner"], ["GET", "/planner/triggers/webhook/secrets"]]) {
    expect((await request(`${base}/modules/bots${suffix}`, method)).status).toBe(403);
  }
  expect(runtimeCalls).toHaveLength(0);
  expect((await request(`${base}/modules/bots`)).status).toBe(200);
});
