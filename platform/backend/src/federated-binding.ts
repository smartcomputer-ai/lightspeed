import { and, eq } from "drizzle-orm";
import { schema, type Db } from "@lightspeed-ai/platform-db";
import { credentialRestrictionsSchema } from "@lightspeed-ai/platform-shared";
import { auditAsSubject } from "./identity-audit.js";
import type { AuthContext } from "./auth-context.js";
import type { WorkloadIdentity } from "./federation.js";

/** An unusable binding still owns its subject and must never be eligible for enrollment. */
export async function resolveFederatedBinding(db: Pick<Db, "select" | "insert" | "update">, identity: WorkloadIdentity) {
  const { federatedCredentials, serviceUsers, organization } = schema;
  const [row] = await db.select({ credential: federatedCredentials, serviceUser: serviceUsers, handle: organization.slug })
    .from(federatedCredentials)
    .innerJoin(serviceUsers, eq(serviceUsers.id, federatedCredentials.serviceUserId))
    .innerJoin(organization, eq(organization.id, serviceUsers.organizationId))
    .where(and(eq(federatedCredentials.issuer, identity.issuer), eq(federatedCredentials.subject, identity.subject)));
  if (!row) return null;
  const limits = credentialRestrictionsSchema.safeParse(row.credential.restrictions);
  if (!limits.success || !row.serviceUser.enabled ||
    (row.credential.serviceAccountUid !== null && row.credential.serviceAccountUid !== identity.serviceAccountUid)) {
    return { auth: null };
  }
  const { issuer, subject } = identity;
  const auth: AuthContext = {
    principal: { kind: "serviceUser", id: row.serviceUser.id, serviceUser: row.serviceUser,
      organizationId: row.serviceUser.organizationId, platformAdmin: false },
    credential: { kind: "federated", id: row.credential.id, restrictions: limits.data, workload: { issuer, subject } },
    actorId: `serviceUser:${row.serviceUser.id}`,
  };
  await db.update(federatedCredentials).set({ lastUsedAt: new Date() }).where(eq(federatedCredentials.id, row.credential.id));
  await auditAsSubject(db, auth, { action: "serviceUser.authenticate", targetId: row.serviceUser.id });
  return { auth, handle: row.handle, organizationId: row.serviceUser.organizationId };
}
