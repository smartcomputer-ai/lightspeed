import { and, eq } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { schema, type Db } from "@lightspeed-ai/platform-db";
import type { UniverseRole } from "@lightspeed-ai/platform-shared";
import type { ServerEnv } from "./env.js";
import { deploymentClient } from "./runtime-client.js";

const testUniverseId = "6c696768-7473-4065-8064-000000000010";
export const developmentOrganizationId = "6c696768-7473-4065-8064-000000000014";
const testUsers = [
  { role: "operator", name: "Operator", id: "6c696768-7473-4065-8064-000000000011" },
  { role: "contributor", name: "Contributor", id: "6c696768-7473-4065-8064-000000000012" },
  { role: "viewer", name: "Viewer", id: "6c696768-7473-4065-8064-000000000013" },
] as const;

/** One shared development organization; developers can create independent universes. */
export async function seedDevelopment(db: Db, env: ServerEnv): Promise<void> {
  if (!env.devSeed) return;
  if (!env.adminEmail || !env.adminPassword) throw new Error("development seeding requires a bootstrap admin email and password");
  const [admin] = await db.select().from(schema.user).where(eq(schema.user.email, env.adminEmail)).limit(1);
  if (!admin) throw new Error("development seeding requires an existing bootstrap admin login");
  const [organization] = await db.select().from(schema.organization).where(eq(schema.organization.id, developmentOrganizationId)).limit(1);
  if (!organization) await db.insert(schema.organization).values({ id: developmentOrganizationId, name: "Development", slug: "development", defaultProtected: false, requireProtected: false, createdAt: new Date() });
  const client = deploymentClient(env);
  const created = await client.call("deployment/universes/create", { universeId: testUniverseId, handle: "test" });
  const [prior] = await db.select().from(schema.universes).where(eq(schema.universes.lightspeedUniverseId, testUniverseId)).limit(1);
  if (prior && prior.organizationId !== developmentOrganizationId) throw new Error("development Test universe must belong to the development organization");
  const universe = prior ?? (await db.insert(schema.universes).values({ organizationId: developmentOrganizationId, name: "Test", handle: created.universe.handle, lightspeedUniverseId: testUniverseId, createdBy: admin.id, features: { bots: true, channels: true }, runtimeConfiguration: { protected: false } }).returning())[0]!;
  await db.update(schema.universes).set({ handle: created.universe.handle }).where(eq(schema.universes.id, universe.id));
  await ensureMember(db, admin.id, "admin"); await ensureGrant(db, universe.id, admin.id, "owner");
  for (const fixture of testUsers) {
    const email = `${fixture.role}@lightspeed.dev`;
    const [existing] = await db.select().from(schema.user).where(eq(schema.user.email, email)).limit(1);
    const userId = existing?.id ?? fixture.id;
    if (!existing) {
      const password = await hashPassword(env.adminPassword);
      await db.transaction(async tx => {
        await tx.insert(schema.user).values({ id: fixture.id, name: fixture.name, email, emailVerified: true });
        await tx.insert(schema.account).values({ id: crypto.randomUUID(), accountId: fixture.id, providerId: "credential", userId: fixture.id, password });
      });
    }
    await ensureMember(db, userId, "creator"); await ensureGrant(db, universe.id, userId, fixture.role);
  }
  console.log("[development] Shared development organization and Test universe ready");
}
async function ensureMember(db: Db, userId: string, role: "admin" | "creator") {
  const [current] = await db.select().from(schema.member).where(and(eq(schema.member.organizationId, developmentOrganizationId), eq(schema.member.userId, userId))).limit(1);
  if (current) await db.update(schema.member).set({ role }).where(eq(schema.member.id, current.id));
  else await db.insert(schema.member).values({ id: crypto.randomUUID(), organizationId: developmentOrganizationId, userId, role, createdAt: new Date() });
}
async function ensureGrant(db: Db, universeId: string, userId: string, role: UniverseRole) {
  const [current] = await db.select().from(schema.universeGrants).where(and(eq(schema.universeGrants.universeId, universeId), eq(schema.universeGrants.userId, userId))).limit(1);
  if (current) await db.update(schema.universeGrants).set({ role }).where(eq(schema.universeGrants.id, current.id));
  else await db.insert(schema.universeGrants).values({ universeId, userId, role });
}
