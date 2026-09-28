import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());

function fixture(role: string, existsInRuntime = true) {
  const calls: string[] = [];
  const writes: { table: unknown; values: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push(rpc.method);
    return Response.json(existsInRuntime
      ? { id: rpc.id, result: { result: { universe: { universeId: rpc.params.universeId } }, notifications: [] } }
      : { id: rpc.id, error: { code: -32004, message: "not found", data: { kind: "not_found", message: "not found" } } });
  }));
  const chain = { from: () => chain, where: () => chain, limit: async () => [] };
  const db = {
    select: () => chain,
    insert: (table: unknown) => ({ values: (values: Record<string, unknown>) => {
      writes.push({ table, values });
      return { then: (resolve: () => unknown) => Promise.resolve().then(resolve), returning: async () => [values] };
    } }),
  };
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "operator", role } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", universeRoutes({
    db: { ...db, transaction: async (work: (tx: typeof db) => unknown) => work(db) },
    env: { lightspeedApiUrl: "https://runtime.example/rpc", lightspeedApiKey: "lsk_platform" },
  } as unknown as AppContext));
  const request = () => app.request("/adopt", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: "Existing runtime", lightspeedUniverseId: "11111111-1111-4111-8111-111111111111" }) });
  return { request, writes, calls };
}

it("adds Platform records around an existing runtime universe without mutating runtime resources", async () => {
  const f = fixture("admin");
  const response = await f.request();
  expect(response.status).toBe(201);
  expect(f.calls).toEqual(["deployment/universes/read"]);
  expect(f.writes.find(w => w.table === schema.universes)?.values).toMatchObject({ lightspeedUniverseId: "11111111-1111-4111-8111-111111111111" });
  expect(f.writes.find(w => w.table === schema.member)?.values).toMatchObject({ userId: "operator", role: "admin" });
});
it("refuses adoption without Platform administration or an existing runtime universe", async () => {
  const forbidden = fixture("user");
  expect((await forbidden.request()).status).toBe(403);
  expect(forbidden.writes).toEqual([]);
  const missing = fixture("admin", false);
  expect((await missing.request()).status).toBe(404);
  expect(missing.writes).toEqual([]);
});
