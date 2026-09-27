import { and, count, eq } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ServerEnv } from "./env.js";
import { auditIdentity } from "./identity-audit.js";

const { user, account } = schema;

/// Seeds the first platform admin from env when the users table is empty.
/// Also designates an existing local password admin for emergency access.
/// Company users are admitted through OIDC; other local accounts use the admin API.
export async function bootstrapAdmin(db: Db, env: ServerEnv): Promise<void> {
  if (!env.adminEmail) {
    return;
  }
  const email = env.adminEmail.toLowerCase();
  // Deployment configuration explicitly designates the existing bootstrap
  // account too. Never turn a company identity or ordinary user into one.
  const [existing] = await db.select().from(user).where(eq(user.email, email));
  if (existing) {
    const [credential] = await db.select({ id: account.id }).from(account)
      .where(and(eq(account.userId, existing.id), eq(account.providerId, "credential")));
    if (existing.identitySource === "local" && existing.role === "admin" && credential && !existing.emergencyAdmin) {
      await db.transaction(async (tx) => {
        await tx.update(user).set({ emergencyAdmin: true }).where(eq(user.id, existing.id));
        await auditIdentity(tx, { action: "emergency.designate", targetId: existing.id });
      });
    }
    return;
  }
  if (!env.adminPassword) return;
  const [{ value: users }] = (await db
    .select({ value: count() })
    .from(user)) as [{ value: number }];
  if (users > 0) {
    return;
  }
  const userId = crypto.randomUUID();
  const now = new Date();
  await db.transaction(async (tx) => {
    await tx.insert(user).values({
      id: userId,
      name: env.adminEmail!.split("@")[0] ?? "admin",
      email,
      emailVerified: true,
      role: "admin",
      emergencyAdmin: true,
      createdAt: now,
      updatedAt: now,
    });
    await tx.insert(account).values({
      id: crypto.randomUUID(),
      accountId: userId,
      providerId: "credential",
      userId,
      password: await hashPassword(env.adminPassword!),
      createdAt: now,
      updatedAt: now,
    });
    await auditIdentity(tx, { action: "emergency.create", targetId: userId });
  });
  console.log(`[bootstrap] seeded platform admin ${env.adminEmail}`);
}
