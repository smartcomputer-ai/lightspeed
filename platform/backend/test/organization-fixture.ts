import { Hono } from "hono";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { PlatformModule } from "@lightspeed-ai/platform-shared";
import type { AuthContext } from "../src/auth-context.js";
import type { ApiVariables, AppContext } from "../src/context.js";
import { universeRoutes } from "../src/routes/universes.js";
import { runtimeHttpRoutes } from "../src/runtime-http.js";
import { platformRoutes } from "../src/routes/platform.js";
import { identityEnv, identityFixture } from "./identity-fixture.js";
import { userSubject } from "./subject-fixture.js";

export async function organizationFixture(modules: readonly PlatformModule[] = []) {
  const fixture = await identityFixture({ ...identityEnv, oidc: null, personalOrganizations: false, lightspeedApiUrl: "https://runtime.example/api/v1", lightspeedApiToken: "organization-fixture-runtime-token-1234567890" });
  const context = { ...fixture, pool: {} as never } as AppContext;
  for (const id of ["owner", "creator", "viewer", "other", "organization-admin"]) await fixture.db.insert(schema.user).values({ id, name: id, email: `${id}@example.test`, emailVerified: true }).onConflictDoNothing();
  await fixture.db.insert(schema.organization).values([{ id: "organization-a", name: "A", slug: "a", createdAt: new Date(), defaultProtected: false, universeQuota: 2 }, { id: "organization-b", name: "B", slug: "b", createdAt: new Date(), defaultProtected: false }]);
  for (const [id, role] of [["owner", "member"], ["creator", "creator"], ["viewer", "member"], ["organization-admin", "admin"]]) await fixture.db.insert(schema.member).values({ id: `member-${id}`, organizationId: "organization-a", userId: id!, role: role!, createdAt: new Date() });
  await fixture.db.insert(schema.member).values({ id: "other-organization", organizationId: "organization-b", userId: "other", role: "admin", createdAt: new Date() });
  const [universe] = await fixture.db.insert(schema.universes).values({ organizationId: "organization-a", name: "Work", handle: "work", lightspeedUniverseId: "11111111-1111-4111-8111-111111111111", createdBy: "owner", runtimeConfiguration: { protected: false } }).returning();
  const [ownerGrant] = await fixture.db.insert(schema.universeGrants).values({ universeId: universe!.id, userId: "owner", role: "owner" }).returning();
  await fixture.db.insert(schema.universeGrants).values({ universeId: universe!.id, userId: "viewer", role: "viewer" });
  let subject = userSubject("owner");
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => { c.set("auth", subject); await next(); });
  app.route("/api/ui/universes", universeRoutes(context));
  app.route("/api/v1/platform", platformRoutes(context, modules));
  app.route("/api/v1", runtimeHttpRoutes(context));
  return { ...fixture, context, universe: universe!, ownerGrant: ownerGrant!,
    setSubject(auth: AuthContext) { subject = auth; },
    async asUser(id: string, platformAdmin = false) { const [user] = await fixture.db.select().from(schema.user).where(eq(schema.user.id, id)); subject = userSubject(id, platformAdmin, user); return subject; },
    request(path: string, method = "GET", body?: unknown) { return app.request(path.startsWith("/api/") ? path : `/api/v1/platform${path}`, { method, headers: { "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) }); },
  };
}
