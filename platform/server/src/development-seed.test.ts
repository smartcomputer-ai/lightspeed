import { afterEach, expect, it, vi } from "vitest";
import { schema, type Db } from "@lightspeed/platform-db";
import { hashPassword } from "better-auth/crypto";
import type { AccessChange } from "@lightspeed-ai/agent-client";
import type { ServerEnv } from "./env.js";
import { seedDevelopment } from "./development-seed.js";

vi.mock("better-auth/crypto", () => ({ hashPassword: vi.fn(async () => "hashed-password") }));
afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

const admin = { id: "admin-login", corePrincipalId: "admin-principal" };
const env = {
  devSeed: true, adminEmail: "admin@lightspeed.dev", adminPassword: "shared-custom-password",
  lightspeedApiUrl: "http://localhost/rpc", lightspeedApiKey: "lsk_fixture",
} as ServerEnv;

function setup(rows: unknown[][], roles = ["deployment_admin"]) {
  const changes: AccessChange[] = [];
  const creations: unknown[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: unknown, init: RequestInit) => {
    expect(new Headers(init.headers).get("x-lightspeed-principal")).toBe(`user:${admin.corePrincipalId}`);
    const rpc = JSON.parse(String(init.body));
    let result: unknown;
    if (rpc.method === "deployment/identity/self") result = { access: { roles } };
    else if (rpc.method === "deployment/universes/create") {
      creations.push(rpc.params); result = { created: true };
    } else {
      expect(rpc.method).toBe("deployment/identity/apply");
      changes.push(rpc.params); result = { changed: true, policyRevision: 1 };
    }
    return Response.json({ id: rpc.id, result: { result, notifications: [] } });
  }));
  const inserts: { table: unknown; values: Record<string, unknown> }[] = [];
  const query = { from: () => query, where: () => query, limit: async () => {
    const next = rows.shift();
    if (!next) throw new Error("unexpected database read");
    return next;
  } };
  const db = {
    select: vi.fn(() => query),
    insert: (table: unknown) => ({ values: async (values: Record<string, unknown>) => { inserts.push({ table, values }); } }),
    transaction: async (fn: (tx: Db) => Promise<void>) => fn(db as unknown as Db),
  };
  return { db: db as unknown as Db, inserts, changes, creations };
}

it("does nothing unless explicitly enabled", async () => {
  const state = setup([]);
  await seedDevelopment(state.db, { ...env, devSeed: false });
  expect(state.db.select).not.toHaveBeenCalled();
  expect(fetch).not.toHaveBeenCalled();
});

it("creates Test and three credential logins with only their universe roles, then safely reruns", async () => {
  const rows: unknown[][] = [[admin], [], [], [], []];
  const state = setup(rows);
  await seedDevelopment(state.db, env);
  const universe = state.inserts.find((entry) => entry.table === schema.universes)!.values;
  expect(universe).toMatchObject({ name: "Test", slug: "test" });
  const users = state.inserts.filter((entry) => entry.table === schema.user).map((entry) => entry.values);
  expect(users.map((u) => u.email)).toEqual(["operator@lightspeed.dev", "contributor@lightspeed.dev", "viewer@lightspeed.dev"]);
  const accounts = state.inserts.filter((entry) => entry.table === schema.account).map((entry) => entry.values);
  expect(accounts).toHaveLength(3);
  expect(accounts.map((a) => a.userId)).toEqual(users.map((u) => u.id));
  expect(accounts.every((a) => a.providerId === "credential" && a.password === "hashed-password")).toBe(true);
  expect(hashPassword).toHaveBeenCalledTimes(3);
  expect(hashPassword).toHaveBeenCalledWith(env.adminPassword);
  const assignments = state.changes.filter((c) => c.operation === "assign_role");
  expect(assignments.map((c) => c.assignment.role)).toEqual(["admin", "operator", "contributor", "viewer"]);
  expect(assignments.every((c) => c.assignment.scope.kind === "universe" && c.assignment.scope.universeId === universe.lightspeedUniverseId)).toBe(true);

  rows.push([admin], [universe], ...users.map((u) => [u]));
  state.inserts.length = 0;
  state.changes.length = 0;
  vi.mocked(hashPassword).mockClear();
  await seedDevelopment(state.db, env);
  expect(state.inserts).toEqual([]);
  expect(hashPassword).not.toHaveBeenCalled();
  expect(state.changes).toEqual(assignments);
  expect(state.creations[1]).toEqual(state.creations[0]);
});

it("reuses an existing Test universe and canonical login identities", async () => {
  const state = setup([[admin], [{ slug: "test", lightspeedUniverseId: "existing-universe" }],
    [{ corePrincipalId: "existing-operator" }], [{ corePrincipalId: "existing-contributor" }], [{ corePrincipalId: "existing-viewer" }]]);
  await seedDevelopment(state.db, env);
  expect(state.inserts).toEqual([]);
  expect(state.creations).toEqual([{ universeId: "existing-universe" }]);
  expect(state.changes.filter((c) => c.operation === "assign_role").map((c) => c.assignment.subject.id))
    .toEqual([admin.corePrincipalId, "existing-operator", "existing-contributor", "existing-viewer"]);
});

it("fails before mutations when the configured login is not an administrator", async () => {
  const state = setup([[admin]], []);
  await expect(seedDevelopment(state.db, env)).rejects.toThrow("requires a deployment administrator");
  expect(state.inserts).toEqual([]);
  expect(state.creations).toEqual([]);
  expect(state.changes).toEqual([]);
});

it("retries with the same canonical identity after a failed login transaction", async () => {
  const rows: unknown[][] = [[admin], [], []];
  const state = setup(rows);
  const transaction = vi.spyOn(state.db, "transaction").mockRejectedValueOnce(new Error("database unavailable"));
  await expect(seedDevelopment(state.db, env)).rejects.toThrow("database unavailable");
  const principal = state.changes.find((c) => c.operation === "create_principal");
  const universe = state.inserts.find((entry) => entry.table === schema.universes)!.values;
  transaction.mockRestore();
  rows.push([admin], [universe], [], [], []);
  state.changes.length = 0;
  await seedDevelopment(state.db, env);
  expect(state.changes.find((c) => c.operation === "create_principal")).toEqual(principal);
  expect(state.inserts.filter((entry) => entry.table === schema.user)).toHaveLength(3);
});
