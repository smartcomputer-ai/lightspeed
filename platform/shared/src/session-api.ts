import { z } from "zod";

export const platformSessionViewSchema = z.object({
  principal: z.object({ kind: z.enum(["user", "serviceUser"]), id: z.string(), name: z.string(), email: z.string().nullable() }),
  credential: z.object({ kind: z.enum(["session", "cliSession", "personalKey", "serviceUserKey", "federated"]), id: z.string() }),
  session: z.object({ id: z.string(), createdAt: z.string().datetime(), expiresAt: z.string().datetime() }).nullable(),
});
export const SESSION_OPERATIONS = {
  "platform/session/read": { helper: { name: "readCurrentSession" }, method: "GET", path: "/api/v1/platform/session", params: z.object({}).strict(), result: platformSessionViewSchema },
  "platform/session/revoke": { helper: { name: "revokeCurrentSession" }, method: "DELETE", path: "/api/v1/platform/session", params: z.object({}).strict(), result: z.object({ revoked: z.literal(true) }) },
} as const;
