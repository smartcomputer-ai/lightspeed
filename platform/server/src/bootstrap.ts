import { userClient } from "./runtime-client.js";
import { count, eq } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ServerEnv } from "./env.js";

const { user, account } = schema;

/// Seeds the first platform admin from env when the users table is empty.
/// Signup is disabled (invite-only), so a fresh deployment needs exactly one
/// out-of-band account; everyone else is created through the admin API.
export async function bootstrapAdmin(db: Db, env: ServerEnv): Promise<void> {
  if (!env.adminEmail || !env.adminPassword) {
    return;
  }
  const [{ value: users }] = (await db
    .select({ value: count() })
    .from(user)) as [{ value: number }];
  if (users > 0) {
    return;
  }
  if (!env.adminPrincipalId) throw new Error("LIGHTSPEED_PLATFORM_ADMIN_PRINCIPAL_ID must name a pre-provisioned core deployment administrator");
  const rights = await userClient(env, env.adminPrincipalId).call("deployment/identity/self", { scope: { kind: "deployment" } });
  if (!rights.result.access.roles.includes("deployment_admin")) throw new Error("bootstrap principal must be an active core deployment administrator");
  const userId = crypto.randomUUID();
  const now = new Date();
  await db.transaction(async (tx) => {
    await tx.insert(user).values({
      id: userId,
      name: env.adminEmail!.split("@")[0] ?? "admin",
      email: env.adminEmail!,
      emailVerified: true,
      corePrincipalId: env.adminPrincipalId!,
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
  });
  console.log(`[bootstrap] seeded platform admin ${env.adminEmail}`);
}
