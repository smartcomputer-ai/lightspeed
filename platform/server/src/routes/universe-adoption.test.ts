import { Hono } from "hono";
import { afterEach, expect, it, vi } from "vitest";
import { schema } from "@lightspeed/platform-db";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

afterEach(() => vi.unstubAllGlobals());
const id = "11111111-1111-4111-8111-111111111111";
function fixture({ role = "admin", exists = true, slug = "runtime-slug", conflict = false, returnedSlug }: {
  role?: string; exists?: boolean; slug?: string | null; conflict?: boolean; returnedSlug?: string;
} = {}) {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  const writes: { table: unknown; values: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const rpc = JSON.parse(String(init.body));
    calls.push(rpc);
    const error = !exists ? { code: -32004, message: "not found" }
      : conflict && rpc.method !== "deployment/universes/read" ? { code: -32009, message: "slug conflict" } : null;
    if (error) return Response.json({ id: rpc.id, error });
    return Response.json({ id: rpc.id, result: { result: { universe: {
      universeId: rpc.params.universeId,
      slug: returnedSlug ?? (rpc.method === "deployment/universes/read" ? slug : rpc.params.slug),
    } }, notifications: [] } });
  }));
  const chain = { from: () => chain, innerJoin: () => chain, where: () => chain, limit: async () => [], then: (resolve: (value: unknown[]) => unknown) => Promise.resolve([]).then(resolve) };
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

it("adopts the existing runtime slug, preserving the UUID and resources", async () => {
  const f = fixture();
  const response = await f.request();
  expect(response.status).toBe(201);
  expect((await response.json() as { slug: string }).slug).toBe("runtime-slug");
  expect(f.calls.map(c => c.method)).toEqual(["deployment/universes/read"]);
  expect(f.writes.find(w => w.table === schema.organization)?.values.slug).toBe("runtime-slug");
  expect(f.writes.find(w => w.table === schema.universes)?.values).toMatchObject({ lightspeedUniverseId: id });
  expect(f.writes.find(w => w.table === schema.member)?.values).toMatchObject({ userId: "operator", role: "admin" });
});

it("requires a supplied slug before adopting an unnamed universe", async () => {
  const f = fixture({ slug: null });
  expect((await f.request()).status).toBe(400);
  expect(f.writes).toEqual([]);
  expect(f.calls.map(c => c.method)).toEqual(["deployment/universes/read"]);
  const response = await f.request({ slug: "personal" });
  expect(response.status).toBe(201);
  expect(f.calls.at(-1)).toMatchObject({ method: "deployment/universes/slug/put", params: { universeId: id, slug: "personal", onlyIfUnset: true } });
  expect((await response.json() as { slug: string }).slug).toBe("personal");
});

it("does not rename an existing universe during adoption", async () => {
  const f = fixture();
  expect((await f.request({ slug: "different" })).status).toBe(409);
  expect(f.writes).toEqual([]);
  expect(f.calls).toHaveLength(1);
});

it("carries creation input to runtime and caches only the runtime response", async () => {
  const f = fixture({ returnedSlug: "authoritative" });
  const response = await f.request({ slug: "requested" }, "/");
  expect(response.status).toBe(201);
  expect(f.calls[0]).toMatchObject({ method: "deployment/universes/create", params: { slug: "requested" } });
  expect((await response.json() as { slug: string }).slug).toBe("authoritative");
  expect(f.writes.find(w => w.table === schema.organization)?.values.slug).toBe("authoritative");
});

it("rejects collisions and racing adoption without suffixes or Platform records", async () => {
  for (const path of ["/", "/adopt"]) {
    const f = fixture({ slug: null, conflict: true });
    expect((await f.request({ slug: "taken" }, path)).status).toBe(409);
    expect(f.writes).toEqual([]);
    expect(f.calls.at(-1)?.params.slug).toBe("taken");
  }
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
