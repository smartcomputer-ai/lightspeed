import { z } from "zod";

const ids = z.array(z.string().min(1).max(200)).max(1000);
export const credentialRestrictionsSchema = z.object({
  organizationIds: ids.optional(),
  universeIds: z.array(z.uuid()).max(1000).optional(),
  operations: z.array(z.string().min(1).max(200)).max(1000).optional(),
  readOnly: z.boolean().optional(),
  allowUniverseCreation: z.boolean().optional(),
}).strict();
export const credentialCreateSchema = z.object({
  name: z.string().trim().min(1).max(120),
  expiresInDays: z.number().int().min(1).max(3650).nullable().optional().describe("Days until the key expires. Omitted uses the deployment default; null creates a key that never expires, which a deployment maximum refuses."),
  restrictions: credentialRestrictionsSchema.default({}),
}).strict();
export const credentialViewSchema = z.object({
  id: z.string(), name: z.string(), prefix: z.string().nullable(), start: z.string().nullable(),
  createdAt: z.string(), expiresAt: z.string().nullable().describe("Null for a key that never expires."), enabled: z.boolean(), lastUsedAt: z.string().nullable(),
  restrictions: credentialRestrictionsSchema,
});
/// A key that can reach an organization, with whose key it is.
export const organizationCredentialViewSchema = credentialViewSchema.extend({
  owner: z.discriminatedUnion("kind", [
    z.object({ kind: z.literal("user"), id: z.string(), name: z.string(), email: z.string() }),
    z.object({ kind: z.literal("serviceUser"), id: z.string(), name: z.string(), enabled: z.boolean() }),
  ]),
});
const created = z.object({ credential: credentialViewSchema, secret: z.string() });
const listed = z.object({ credentials: z.array(credentialViewSchema) });
const revoked = z.object({ revoked: z.literal(true) });
const serviceUserPath = { organizationId: z.string().min(1), serviceUserId: z.uuid() };
const keyId = z.string().min(1);

export const CREDENTIAL_OPERATIONS = {
  "platform/credentials/list": { helper: { name: "listPersonalApiKeys" }, method: "GET", path: "/api/v1/platform/credentials", params: z.object({}).strict(), result: listed },
  "platform/credentials/create": { helper: { name: "createPersonalApiKey" }, method: "POST", path: "/api/v1/platform/credentials", params: credentialCreateSchema, result: created },
  "platform/credentials/revoke": { helper: { name: "revokePersonalApiKey" }, method: "DELETE", path: "/api/v1/platform/credentials/{keyId}", params: z.object({ keyId }).strict(), result: revoked },
  "platform/organizations/credentials/list": { helper: { name: "listOrganizationApiKeys" }, method: "GET", path: "/api/v1/platform/organizations/{organizationId}/credentials", params: z.object({ organizationId: z.string().min(1) }).strict(), result: z.object({ credentials: z.array(organizationCredentialViewSchema).describe("Keys that can reach the organization: its service users' keys, and personal keys of people with a role in it whose restrictions allow it.") }) },
  "platform/service-users/credentials/list": { helper: { name: "listServiceUserApiKeys" }, method: "GET", path: "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}/credentials", params: z.object(serviceUserPath).strict(), result: listed },
  "platform/service-users/credentials/create": { helper: { name: "createServiceUserApiKey" }, method: "POST", path: "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}/credentials", params: credentialCreateSchema.extend(serviceUserPath), result: created },
  "platform/service-users/credentials/revoke": { helper: { name: "revokeServiceUserApiKey" }, method: "DELETE", path: "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}/credentials/{keyId}", params: z.object({ ...serviceUserPath, keyId }).strict(), result: revoked },
} as const;
