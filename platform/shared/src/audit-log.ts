import { z } from "zod";

export const auditQuerySchema = z.object({
  limit: z.string().regex(/^\d+$/).default("50").transform(Number).pipe(z.number().int().min(1).max(100)),
  cursor: z.uuid().transform((id) => id.toLowerCase()).optional(),
});

export interface AuditEvent {
  id: string;
  createdAt: string;
  actorId: string | null;
  action: string;
  targetId: string | null;
  universeId: string | null;
  outcome: string;
  details: Record<string, unknown>;
}

export interface AuditPage {
  events: AuditEvent[];
  nextCursor: string | null;
}
