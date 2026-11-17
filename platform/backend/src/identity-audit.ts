import { schema, type Db } from "@lightspeed-ai/platform-db";

type AuditStore = Pick<Db, "insert">;
export type AuditEvent = Pick<typeof schema.identityAudit.$inferInsert, "actorId" | "action" | "targetId" | "universeId" | "details"> & {
  outcome?: "success" | "denied" | "failed";
};

// Callers supply explicit safe fields, never request bodies, claims or tokens.
export async function auditIdentity(db: AuditStore, event: AuditEvent): Promise<void> {
  await db.insert(schema.identityAudit).values({ ...event, outcome: event.outcome ?? "success" });
}

/** Principal and credential attribution for every Platform-native mutation. */
export async function auditAsSubject(db: AuditStore, auth: import("./auth-context.js").AuthContext, event: Omit<AuditEvent, "actorId">): Promise<void> {
  return auditIdentity(db, { ...event, actorId: auth.actorId, details: { ...event.details, credentialId: auth.credential.id, credentialKind: auth.credential.kind } });
}
