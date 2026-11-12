import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed-ai/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());
const id = "11111111-1111-4111-8111-111111111111";
function fixture({ role = "admin", exists = true, handle = "runtime-handle", conflict = false, cached = false, returnedHandle }: {
  role?: string; exists?: boolean; handle?: string; conflict?: boolean; cached?: boolean; returnedHandle?: string;
} = {}) {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  const writes: { table: unknown; values: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push(rpc);
    const error = !exists ? { code: -32004, message: "not found" }
      : conflict && rpc.method !== "deployment/universes/read" ? { code: -32009, message: "handle conflict" } : null;
    if (error) return Response.json({ id: rpc.id, error });
    return Response.json({ id: rpc.id, result: { result: { universe: {
      universeId: rpc.params.universeId ?? rpc.params.universe,
      handle: returnedHandle ?? (rpc.method === "deployment/universes/read" ? handle : rpc.params.handle),
    } }, notifications: [] } });
  }));
  // The first read finds no linked universe; `cached` makes the handle cache
  // lookup that follows find another organization.
  const reads: unknown[][] = cached ? [[], [{ id: "other-org" }]] : [];
  const next = async () => reads.shift() ?? [];
  const chain = { from: () => chain, innerJoin: () => chain, where: () => chain, limit: next, then: (resolve: (value: unknown[]) => unknown) => next().then(resolve) };
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
  const request = (body: object = {}, path = "/adopt") => app.request(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: "Display name", lightspeedUniverseId: id, ...body }) });
  return { request, writes, calls };
}

it("adopts under the existing runtime handle, preserving the UUID and resources", async () => {
  const f = fixture();
  const response = await f.request();
  expect(response.status).toBe(201);
  expect((await response.json() as { handle: string }).handle).toBe("runtime-handle");
  expect(f.calls).toEqual([expect.objectContaining({ method: "deployment/universes/read", params: { universe: id } })]);
  expect(f.writes.find(w => w.table === schema.organization)?.values.slug).toBe("runtime-handle");
  expect(f.writes.find(w => w.table === schema.universes)?.values).toMatchObject({ lightspeedUniverseId: id });
  expect(f.writes.find(w => w.table === schema.member)?.values).toMatchObject({ userId: "operator", role: "admin" });
});

it("never renames during adoption: a handle in the request is refused", async () => {
  const f = fixture();
  expect((await f.request({ handle: "different" })).status).toBe(400);
  expect(f.writes).toEqual([]);
  expect(f.calls).toEqual([]);
});

it("refuses to adopt a universe whose handle another Platform universe already uses", async () => {
  const f = fixture({ cached: true });
  expect((await f.request()).status).toBe(409);
  expect(f.writes).toEqual([]);
  expect(f.calls.map(c => c.method)).toEqual(["deployment/universes/read"]);
});

it("carries creation input to runtime and caches only the runtime response", async () => {
  const f = fixture({ returnedHandle: "authoritative" });
  const response = await f.request({ handle: "requested" }, "/");
  expect(response.status).toBe(201);
  expect(f.calls[0]).toMatchObject({ method: "deployment/universes/create", params: { handle: "requested" } });
  expect((await response.json() as { handle: string }).handle).toBe("authoritative");
  expect(f.writes.find(w => w.table === schema.organization)?.values.slug).toBe("authoritative");
});

it("derives the handle from the display name when creation names none", async () => {
  const f = fixture();
  expect((await f.request({ name: "Display Name" }, "/")).status).toBe(201);
  expect(f.calls[0]).toMatchObject({ method: "deployment/universes/create", params: { handle: "display-name" } });
});

it("rejects a creation collision without a suffix or Platform records", async () => {
  const f = fixture({ conflict: true });
  expect((await f.request({ handle: "taken" }, "/")).status).toBe(409);
  expect(f.writes).toEqual([]);
  expect(f.calls.at(-1)?.params.handle).toBe("taken");
});

it("refuses adoption without Platform administration or an existing runtime universe", async () => {
  const forbidden = fixture({ role: "user" });
  expect((await forbidden.request()).status).toBe(403);
  expect(forbidden.calls).toEqual([]);
  expect(forbidden.writes).toEqual([]);
  const missing = fixture({ exists: false });
  expect((await missing.request()).status).toBe(404);
  expect(missing.writes).toEqual([]);
});
