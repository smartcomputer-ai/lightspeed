import { z } from "zod";

const ids = z.array(z.string().min(1).max(200)).max(1000);
export const credentialRestrictionsSchema = z.object({
  accountIds: ids.optional(),
  universeIds: z.array(z.uuid()).max(1000).optional(),
  operations: z.array(z.string().min(1).max(200)).max(1000).optional(),
  readOnly: z.boolean().optional(),
  allowUniverseCreation: z.boolean().optional(),
}).strict();
export const credentialCreateSchema = z.object({
  name: z.string().trim().min(1).max(120),
  expiresInDays: z.number().int().min(1).max(3650).optional(),
  restrictions: credentialRestrictionsSchema.default({}),
}).strict();
export const credentialViewSchema = z.object({
  id: z.string(), name: z.string(), prefix: z.string().nullable(), start: z.string().nullable(),
  createdAt: z.string(), expiresAt: z.string(), enabled: z.boolean(), lastUsedAt: z.string().nullable(),
  restrictions: credentialRestrictionsSchema,
});
const created = z.object({ credential: credentialViewSchema, secret: z.string() });
const listed = z.object({ credentials: z.array(credentialViewSchema) });
const revoked = z.object({ revoked: z.literal(true) });
const applicationPath = { accountId: z.string().min(1), applicationId: z.uuid() };
const keyId = z.string().min(1);

export const CREDENTIAL_OPERATIONS = {
  "platform/credentials/list": { helper: { name: "listPersonalApiKeys" }, method: "GET", path: "/api/v1/platform/credentials", params: z.object({}).strict(), result: listed },
  "platform/credentials/create": { helper: { name: "createPersonalApiKey" }, method: "POST", path: "/api/v1/platform/credentials", params: credentialCreateSchema, result: created },
  "platform/credentials/revoke": { helper: { name: "revokePersonalApiKey" }, method: "DELETE", path: "/api/v1/platform/credentials/{keyId}", params: z.object({ keyId }).strict(), result: revoked },
  "platform/applications/credentials/list": { helper: { name: "listApplicationApiKeys" }, method: "GET", path: "/api/v1/platform/accounts/{accountId}/applications/{applicationId}/credentials", params: z.object(applicationPath).strict(), result: listed },
  "platform/applications/credentials/create": { helper: { name: "createApplicationApiKey" }, method: "POST", path: "/api/v1/platform/accounts/{accountId}/applications/{applicationId}/credentials", params: credentialCreateSchema.extend(applicationPath), result: created },
  "platform/applications/credentials/revoke": { helper: { name: "revokeApplicationApiKey" }, method: "DELETE", path: "/api/v1/platform/accounts/{accountId}/applications/{applicationId}/credentials/{keyId}", params: z.object({ ...applicationPath, keyId }).strict(), result: revoked },
} as const;
