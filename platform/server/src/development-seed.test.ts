import { afterEach, expect, it, vi } from "vitest";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ServerEnv } from "./env.js";
import { seedDevelopment } from "./development-seed.js";

vi.mock("better-auth/crypto", () => ({ hashPassword: vi.fn(async () => "hashed-password") }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

const admin = { id: "admin-login", email: "admin@lightspeed.dev" };
const env = {
  devSeed: true, adminEmail: "admin@lightspeed.dev", adminPassword: "shared-custom-password",
  lightspeedApiUrl: "http://localhost/rpc", lightspeedApiKey: "lsk_fixture",
} as ServerEnv;

/// A database answering each read, in order, with the next of `reads`, and a
/// core that accepts universe creation with the deployment key alone.
function setup(reads: unknown[][]) {
  const creations: unknown[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    const headers = new Headers(init.headers);
    expect(headers.get("authorization")).toBe("Bearer lsk_fixture");
    expect(headers.get("x-lightspeed-actor")).toBeNull();
    const rpc = JSON.parse(String(init.body));
    expect(rpc.method).toBe("deployment/universes/create");
    creations.push(rpc.params);
    return Response.json({ id: rpc.id, result: { result: { created: true }, notifications: [] } });
  }));
  const inserts: { table: unknown; values: Record<string, unknown> }[] = [];
  const updates: Record<string, unknown>[] = [];
  const query = { from: () => query, where: () => query, limit: async () => {
    const next = reads.shift();
    if (!next) throw new Error("unexpected database read");
    return next;
  } };
  const db = {
    select: vi.fn(() => query),
    insert: (table: unknown) => ({ values: async (values: Record<string, unknown>) => { inserts.push({ table, values }); } }),
    update: () => ({ set: (values: Record<string, unknown>) => ({ where: async () => { updates.push(values); } }) }),
    transaction: async (fn: (tx: Db) => Promise<void>) => fn(db as unknown as Db),
  };
  return { db: db as unknown as Db, inserts, updates, creations };
}

it("does nothing unless explicitly enabled", async () => {
  const state = setup([]);
  await seedDevelopment(state.db, { ...env, devSeed: false });
  expect(state.db.select).not.toHaveBeenCalled();
  expect(fetch).not.toHaveBeenCalled();
});

it("creates Test with the admin and three credential logins as members of their role", async () => {
  // admin login, no universe row, no admin membership, then per fixture: no
  // login and no membership.
  const state = setup([[admin], [], [], [], [], [], [], [], []]);
  await seedDevelopment(state.db, env);
  expect(state.creations).toEqual([{ universeId: "6c696768-7473-4065-8064-000000000010" }]);
  const values = (table: unknown) => state.inserts.filter((entry) => entry.table === table).map((entry) => entry.values);
  expect(values(schema.organization)).toMatchObject([{ name: "Test", slug: "test" }]);
  const organizationId = values(schema.organization)[0]!.id;
  expect(values(schema.universes)).toMatchObject([{ organizationId, name: "Test" }]);
  expect(values(schema.user).map((user) => user.email)).toEqual([
    "operator@lightspeed.dev", "contributor@lightspeed.dev", "viewer@lightspeed.dev",
  ]);
  expect(values(schema.account)).toHaveLength(3);
  expect(values(schema.member).map(({ userId, role }) => [userId, role])).toEqual([
    ["admin-login", "admin"],
    ["6c696768-7473-4065-8064-000000000011", "operator"],
    ["6c696768-7473-4065-8064-000000000012", "contributor"],
    ["6c696768-7473-4065-8064-000000000013", "viewer"],
  ]);
});

it("reruns without duplicating and restores a changed role", async () => {
  const universe = { organizationId: "org", gatewayUrl: null };
  const state = setup([
    [admin], [universe], [{ id: "a", role: "admin" }],
    [{ id: "op" }], [{ id: "m-op", role: "viewer" }],
    [{ id: "co" }], [{ id: "m-co", role: "contributor" }],
    [{ id: "vi" }], [{ id: "m-vi", role: "viewer" }],
  ]);
  await seedDevelopment(state.db, env);
  expect(state.inserts).toEqual([]);
  expect(state.updates).toEqual([{ role: "operator" }]);
});
