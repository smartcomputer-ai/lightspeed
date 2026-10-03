import { readFile } from "node:fs/promises";
import { PGlite } from "@electric-sql/pglite";
import { drizzle } from "drizzle-orm/pglite";
import { Hono } from "hono";
import { UNIVERSE_ICONS } from "@lightspeed-ai/platform-shared";
import { afterAll, beforeAll, expect, it } from "vitest";
import type { ApiVariables, AppContext } from "../context.js";
import { universeRoutes } from "./universes.js";

const database = new PGlite();
const id = "33333333-3333-4333-8333-333333333333";
const migrations = new URL("../../../db/migrations/", import.meta.url);

beforeAll(async () => {
  const journal = JSON.parse(await readFile(new URL("meta/_journal.json", migrations), "utf8")) as { entries: { tag: string }[] };
  await database.exec(await readFile(new URL(`${journal.entries[0]!.tag}.sql`, migrations), "utf8"));
  // A retained universe must receive the same defaults as a fresh one.
  await database.exec(`
    INSERT INTO "user" (id, name, email) VALUES
      ('admin', 'Admin', 'admin@example.test'), ('viewer', 'Viewer', 'viewer@example.test'),
      ('contributor', 'Contributor', 'contributor@example.test'), ('operator', 'Operator', 'operator@example.test');
    INSERT INTO organization (id, name, slug, created_at) VALUES ('org', 'Test', 'test', now());
    INSERT INTO member (id, organization_id, user_id, role, created_at)
      SELECT id, 'org', id, id, now() FROM "user";
    INSERT INTO universes (id, organization_id, lightspeed_universe_id, name)
      VALUES ('${id}', 'org', '${id}', 'Test');
  `);
  for (const entry of journal.entries.slice(1)) {
    await database.exec(await readFile(new URL(`${entry.tag}.sql`, migrations), "utf8"));
  }
});
afterAll(async () => database.close());

function request(userId: string, method = "GET", body?: unknown, platformAdmin = false, path = `/${id}`) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    c.set("session", { user: { id: userId, role: platformAdmin ? "admin" : "user" } } as ApiVariables["session"]);
    await next();
  });
  app.route("/", universeRoutes({ db: drizzle(database) } as unknown as AppContext));
  return app.request(path, { method, headers: { "content-type": "application/json" },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
}

it("gives retained and newly created universes the default appearance", async () => {
  expect(await (await request("viewer")).json()).toMatchObject({ icon: "orbit", iconColor: "default" });
  await database.exec(`
    INSERT INTO organization (id, name, slug, created_at) VALUES ('new-org', 'New', 'new', now());
    INSERT INTO universes (organization_id, lightspeed_universe_id, name)
      VALUES ('new-org', '44444444-4444-4444-8444-444444444444', 'New');
  `);
  const { rows } = await database.query("SELECT icon, icon_color FROM universes WHERE organization_id = 'new-org'");
  expect(rows).toEqual([{ icon: "orbit", icon_color: "default" }]);
});

it.each(["viewer", "contributor", "operator"])("refuses appearance changes by a %s", async role => {
  expect((await request(role, "PATCH", { icon: "rocket", iconColor: "blue" })).status).toBe(403);
});

it("persists an admin's selection for every member and list read", async () => {
  const response = await request("admin", "PATCH", { icon: "rocket", iconColor: "blue" });
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ icon: "rocket", iconColor: "blue" });
  for (const role of ["viewer", "contributor", "operator", "admin"]) {
    expect(await (await request(role)).json()).toMatchObject({ icon: "rocket", iconColor: "blue" });
    expect(await (await request(role, "GET", undefined, false, "/")).json())
      .toEqual(expect.arrayContaining([expect.objectContaining({ id, icon: "rocket", iconColor: "blue" })]));
  }
});

it("allows a platform admin without membership and preserves omitted appearance fields", async () => {
  expect((await request("platform-admin", "PATCH", { iconColor: "violet" }, true)).status).toBe(200);
  expect((await request("admin", "PATCH", { name: "Renamed" })).status).toBe(200);
  expect(await (await request("viewer")).json()).toMatchObject({ name: "Renamed", icon: "rocket", iconColor: "violet" });
});

it.each([{ icon: "unknown" }, { iconColor: "#ffffff" }, { icon: null }, { iconColor: null }])("rejects unsupported appearance values (%j)", async body => {
  const before = await (await request("viewer")).json();
  expect((await request("admin", "PATCH", body)).status).toBe(400);
  expect(await (await request("viewer")).json()).toEqual(before);
});

it("lets admins restore the default appearance", async () => {
  expect((await request("admin", "PATCH", { icon: "orbit", iconColor: "default" })).status).toBe(200);
  expect(await (await request("viewer")).json()).toMatchObject({ icon: "orbit", iconColor: "default" });
});

it.each(UNIVERSE_ICONS)("persists the %s icon for other members", async icon => {
  expect((await request("admin", "PATCH", { icon })).status).toBe(200);
  expect(await (await request("viewer")).json()).toMatchObject({ icon });
});
