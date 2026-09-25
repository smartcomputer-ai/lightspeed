import { Hono } from "hono";
import { expect, it, vi } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

const universe = { id: "platform-universe", organizationId: "org", lightspeedUniverseId: "33333333-3333-4333-8333-333333333333", gatewayUrl: null };

/// Universe routes over a database that answers each read, in order, with
/// the next of `reads`: first the universe, then the caller's membership.
function setup(role: string | null, reads: unknown[][] = [], platformRole?: string) {
  const queue: unknown[][] = [[{ universe, slug: "test" }], role ? [{ role }] : [], ...reads];
  const next = async () => {
    const rows = queue.shift();
    if (!rows) throw new Error("unexpected database read");
    return rows;
  };
  const chain: Record<string, unknown> = {};
  for (const step of ["from", "innerJoin", "leftJoin", "where", "set"]) chain[step] = () => chain;
  chain.limit = next;
  chain.returning = next;
  chain.then = (resolve: (rows: unknown[]) => unknown, reject: (error: unknown) => unknown) => next().then(resolve, reject);
  const writes: string[] = [];
  const db = {
    select: () => chain,
    insert: () => { writes.push("insert"); return { values: () => chain }; },
    update: () => { writes.push("update"); return chain; },
    delete: () => { writes.push("delete"); return chain; },
  };
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: "caller", role: platformRole } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", universeRoutes({ db } as unknown as AppContext));
  const request = (path: string, method = "GET", body?: unknown) => app.request(`/platform-universe${path}`, {
    method, headers: { "content-type": "application/json" }, ...(body ? { body: JSON.stringify(body) } : {}),
  });
  return { request, writes, queue };
}

const members = [
  { id: "m1", userId: "caller", role: "viewer", email: "caller@example.test", name: "Caller", createdAt: null },
  { id: "m2", userId: "admin", role: "admin", email: "admin@example.test", name: "Admin", createdAt: null },
];

it("hides a universe from non-members", async () => {
  const { request } = setup(null);
  expect((await request("")).status).toBe(404);
});

it("shows members and roles to every member, emails to admins", async () => {
  const viewer = await (await setup("viewer", [members]).request("/members")).json() as Record<string, unknown>[];
  expect(viewer[0]).not.toHaveProperty("email");
  expect(viewer[1]).toMatchObject({ role: "admin", name: "Admin" });
  const admin = await (await setup("admin", [members]).request("/members")).json() as Record<string, unknown>[];
  expect(admin[1]).toMatchObject({ email: "admin@example.test" });
});

it("treats a platform admin as a universe admin", async () => {
  const { request } = setup(null, [members], "admin");
  expect((await request("/members")).status).toBe(200);
});

it.each(["viewer", "contributor", "operator"])("keeps membership changes with admins, refusing a %s", async (role) => {
  const { request, writes } = setup(role);
  expect((await request("/members", "POST", { userId: "new", role: "viewer" })).status).toBe(403);
  expect((await setup(role).request("/members/m2", "PATCH", { role: "viewer" })).status).toBe(403);
  expect((await setup(role).request("/members/m2", "DELETE")).status).toBe(403);
  expect(writes).toEqual([]);
});

it("refuses a role outside the four", async () => {
  const { request, writes } = setup("admin");
  expect((await request("/members", "POST", { userId: "new", role: "owner" })).status).toBe(400);
  expect(writes).toEqual([]);
});

it("keeps the last admin: no demotion, no removal", async () => {
  const demote = setup("admin", [[{ role: "admin" }], []]);
  expect((await demote.request("/members/m2", "PATCH", { role: "operator" })).status).toBe(409);
  const remove = setup("admin", [[{ role: "admin" }], []]);
  expect((await remove.request("/members/m2", "DELETE")).status).toBe(409);
  expect([...demote.writes, ...remove.writes]).toEqual([]);
});

it("changes and removes an admin while another remains", async () => {
  const demote = setup("admin", [[{ role: "admin" }], [{ id: "m3" }], [{ id: "m2", role: "operator" }]]);
  const response = await demote.request("/members/m2", "PATCH", { role: "operator" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ role: "operator" });
  const remove = setup("admin", [[{ role: "admin" }], [{ id: "m3" }], [{ id: "m2" }]]);
  expect((await remove.request("/members/m2", "DELETE")).status).toBe(200);
  expect(remove.writes).toEqual(["delete"]);
});
