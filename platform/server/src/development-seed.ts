import { eq, or } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ServerEnv } from "./env.js";
import { userClient } from "./runtime-client.js";

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
  const client = userClient(env, admin.corePrincipalId);
  const rights = await client.call("deployment/identity/self", { scope: { kind: "deployment" } });
  if (!rights.result.access.roles.includes("deployment_admin")) {
    throw new Error("development seeding requires a deployment administrator");
  }

  const [existing] = await db.select().from(schema.universes).where(or(
    eq(schema.universes.lightspeedUniverseId, testUniverseId), eq(schema.universes.slug, "test"),
  )).limit(1);
  if (existing?.gatewayUrl && existing.gatewayUrl !== env.lightspeedApiUrl) {
    throw new Error("development Test universe must use the local runtime");
  }
  const universeId = existing?.lightspeedUniverseId ?? testUniverseId;
  // Core creation and role assignment are idempotent, including after a partial seed.
  await client.call("deployment/universes/create", { universeId });
  if (!existing) {
    await db.insert(schema.universes).values({ name: "Test", slug: "test", lightspeedUniverseId: universeId });
  }
  const scope = { kind: "universe" as const, universeId };
  await client.call("deployment/identity/apply", {
    operation: "assign_role", assignment: { scope, subject: { kind: "principal", id: admin.corePrincipalId }, role: "admin" },
  });

  for (const fixture of testUsers) {
    const email = `${fixture.role}@lightspeed.dev`;
    const [existingUser] = await db.select().from(schema.user).where(or(
      eq(schema.user.id, fixture.id), eq(schema.user.email, email),
    )).limit(1);
    const principalId = existingUser?.corePrincipalId ?? fixture.id;
    if (!existingUser) {
      // Stable principal IDs allow retrying if the login transaction fails after
      // the core identity was committed. Never resolve core identities by email.
      await client.call("deployment/identity/apply", {
        operation: "create_principal", id: principalId, kind: "user",
        displayName: fixture.name, managementScope: { kind: "deployment" },
      });
      const password = await hashPassword(env.adminPassword);
      await db.transaction(async (tx) => {
        await tx.insert(schema.user).values({ id: fixture.id, corePrincipalId: principalId,
          name: fixture.name, email, emailVerified: true });
        await tx.insert(schema.account).values({ id: crypto.randomUUID(), accountId: fixture.id,
          providerId: "credential", userId: fixture.id, password });
      });
    }
    await client.call("deployment/identity/apply", {
      operation: "assign_role", assignment: { scope, subject: { kind: "principal", id: principalId }, role: fixture.role },
    });
  }
  console.log("[development] Test universe and Admin, Operator, Contributor, Viewer accounts ready");
}
