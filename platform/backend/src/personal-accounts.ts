import { randomUUID } from "node:crypto";
import { and, eq } from "drizzle-orm";
import { schema, type Db } from "@lightspeed-ai/platform-db";
import { auditIdentity } from "./identity-audit.js";
import type { ServerEnv } from "./env.js";

/** Serialize on the user so concurrent sign-ins create one durable personal account. */
export async function ensurePersonalAccount(db: Db, env: ServerEnv, userId: string): Promise<void> {
  if (env.personalAccounts === false && !env.devSeed) return;
  await db.transaction(async (tx) => {
    const [user] = await tx.select().from(schema.user).where(eq(schema.user.id, userId)).for("update");
    if (!user || user.banned || (user.identitySource === "company" && !user.companyAdmitted)) return;
    if (env.devSeed) {
      const [development] = await tx.select().from(schema.organization).where(and(eq(schema.organization.slug, "development"), eq(schema.organization.kind, "development")));
      const [provisioned] = await tx.select({ id: schema.identityAudit.id }).from(schema.identityAudit)
        .where(and(eq(schema.identityAudit.action, "account.development_admission"), eq(schema.identityAudit.targetId, userId))).limit(1);
      if (development?.enabled && !provisioned) {
        await tx.insert(schema.member).values({ id: randomUUID(), organizationId: development.id, userId, role: "creator", createdAt: new Date() })
          .onConflictDoNothing({ target: [schema.member.organizationId, schema.member.userId] });
        // This durable admission marker prevents later membership removal from being undone.
        await auditIdentity(tx, { actorId: userId, action: "account.development_admission", targetId: userId, details: { accountId: development.id } });
      }
    }
    if (env.personalAccounts === false) return;
    const [existing] = await tx.select({ id: schema.organization.id }).from(schema.organization)
      .where(eq(schema.organization.personalUserId, userId));
    if (existing) return;
    const id = randomUUID();
    await tx.insert(schema.organization).values({
      id, name: `${user.name}'s account`, slug: `personal-${id}`, kind: "personal", personalUserId: userId,
      enabled: true, universeQuota: env.personalAccountUniverseQuota ?? 25, defaultProtected: false,
      requireProtected: false, createdAt: new Date(),
    });
    await tx.insert(schema.member).values({ id: randomUUID(), organizationId: id, userId, role: "admin", createdAt: new Date() });
  });
}
