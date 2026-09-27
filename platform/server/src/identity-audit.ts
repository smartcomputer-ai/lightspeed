import { schema, type Db } from "@lightspeed/platform-db";

type AuditStore = Pick<Db, "insert">;
export type AuditEvent = Pick<typeof schema.identityAudit.$inferInsert, "actorId" | "action" | "targetId" | "universeId" | "details"> & {
  outcome?: "success" | "denied" | "failed";
};

// Callers supply explicit safe fields, never request bodies, claims or tokens.
export async function auditIdentity(db: AuditStore, event: AuditEvent): Promise<void> {
  await db.insert(schema.identityAudit).values({ ...event, outcome: event.outcome ?? "success" });
}
