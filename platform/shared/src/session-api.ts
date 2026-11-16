import { z } from "zod";

export const platformSessionViewSchema = z.object({
  principal: z.object({ kind: z.enum(["user", "application"]), id: z.string(), name: z.string(), email: z.string().nullable() }),
  credential: z.object({ kind: z.enum(["session", "cliSession", "personalKey", "applicationKey"]), id: z.string() }),
  session: z.object({ id: z.string(), createdAt: z.string().datetime(), expiresAt: z.string().datetime() }).nullable(),
});
export const SESSION_OPERATIONS = {
  "platform/session/read": { method: "GET", path: "/api/v1/platform/session", params: z.object({}).strict(), result: platformSessionViewSchema },
  "platform/session/revoke": { method: "DELETE", path: "/api/v1/platform/session", params: z.object({}).strict(), result: z.object({ revoked: z.literal(true) }) },
} as const;
