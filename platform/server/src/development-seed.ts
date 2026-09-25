import { and, eq } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { schema, type Db } from "@lightspeed/platform-db";
import type { UniverseRole } from "@lightspeed/platform-shared";
import type { ServerEnv } from "./env.js";
import { deploymentClient } from "./runtime-client.js";

const testUniverseId = "6c696768-7473-4065-8064-000000000010";
const testUsers = [
  { role: "operator", name: "Operator", id: "6c696768-7473-4065-8064-000000000011" },
  { role: "contributor", name: "Contributor", id: "6c696768-7473-4065-8064-000000000012" },
  { role: "viewer", name: "Viewer", id: "6c696768-7473-4065-8064-000000000013" },
] as const;

/** Explicit launcher-only fixtures; normal Platform startup never seeds these. */
export async function seedDevelopment(db: Db, env: ServerEnv): Promise<void> {
  if (!env.devSeed) return;
  if (!env.adminEmail || !env.adminPassword) throw new Error("development seeding requires a bootstrap admin email and password");
  const [admin] = await db.select().from(schema.user).where(eq(schema.user.email, env.adminEmail)).limit(1);
  if (!admin) throw new Error("development seeding requires an existing bootstrap admin login");

  // Core creation is idempotent, including after a partial seed.
  await deploymentClient(env).call("deployment/universes/create", { universeId: testUniverseId });
  const [existing] = await db.select().from(schema.universes)
    .where(eq(schema.universes.lightspeedUniverseId, testUniverseId)).limit(1);
  if (existing?.gatewayUrl && existing.gatewayUrl !== env.lightspeedApiUrl) {
    throw new Error("development Test universe must use the local runtime");
  }
  let organizationId = existing?.organizationId;
  if (!organizationId) {
    organizationId = crypto.randomUUID();
    await db.transaction(async (tx) => {
      await tx.insert(schema.organization).values({ id: organizationId!, name: "Test", slug: "test", createdAt: new Date() });
      await tx.insert(schema.universes).values({ organizationId: organizationId!, name: "Test", lightspeedUniverseId: testUniverseId });
    });
  }
  await ensureMember(db, organizationId, admin.id, "admin");

  for (const fixture of testUsers) {
    const email = `${fixture.role}@lightspeed.dev`;
    const [existingUser] = await db.select().from(schema.user).where(eq(schema.user.email, email)).limit(1);
    let userId = existingUser?.id;
    if (!userId) {
      userId = fixture.id;
      const password = await hashPassword(env.adminPassword);
      await db.transaction(async (tx) => {
        await tx.insert(schema.user).values({ id: fixture.id, name: fixture.name, email, emailVerified: true });
        await tx.insert(schema.account).values({ id: crypto.randomUUID(), accountId: fixture.id,
          providerId: "credential", userId: fixture.id, password });
      });
    }
    await ensureMember(db, organizationId, userId, fixture.role);
  }
  console.log("[development] Test universe and Admin, Operator, Contributor, Viewer accounts ready");
}

/// The user is a member with exactly `role`.
async function ensureMember(db: Db, organizationId: string, userId: string, role: UniverseRole): Promise<void> {
  const where = and(eq(schema.member.organizationId, organizationId), eq(schema.member.userId, userId));
  const [membership] = await db.select().from(schema.member).where(where).limit(1);
  if (!membership) {
    await db.insert(schema.member).values({ id: crypto.randomUUID(), organizationId, userId, role, createdAt: new Date() });
  } else if (membership.role !== role) {
    await db.update(schema.member).set({ role }).where(eq(schema.member.id, membership.id));
  }
}
