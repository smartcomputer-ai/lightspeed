import { z } from "zod";
import { CREDENTIAL_OPERATIONS, credentialRestrictionsSchema } from "./credential-api.js";
import { SESSION_OPERATIONS, platformSessionViewSchema } from "./session-api.js";

export const accountRoleSchema = z.enum(["member", "creator", "admin"]);
export type AccountRole = z.infer<typeof accountRoleSchema>;
/** Platform's extension to common connection discovery; core owns the common fields. */
export const platformConnectionDetailsSchema = z.object({
  principal: platformSessionViewSchema.shape.principal.omit({ email: true }).extend({
    platformAdmin: z.boolean().describe("Whether the principal is a Platform Admin. Credential restrictions still apply to every operation."),
  }),
  credential: platformSessionViewSchema.shape.credential,
  accounts: z.array(z.object({ id: z.string(), name: z.string(), role: accountRoleSchema })),
  restrictions: credentialRestrictionsSchema,
});
export type PlatformConnectionDetails = z.infer<typeof platformConnectionDetailsSchema>;
export const principalRefSchema = z.object({ kind: z.enum(["user", "application"]), id: z.string().min(1) }).strict();
export type PrincipalRef = z.infer<typeof principalRefSchema>;
const universeRole = z.enum(["viewer", "contributor", "operator", "owner"]);
const id = z.string().min(1);
const name = z.string().trim().min(1).max(120);
const handle = z.string().regex(/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/);
const timestamp = z.string().datetime();
function declaredMemberSchema<T extends z.ZodType>(role: T) {
  return z.union([z.object({ email: z.email(), role }).strict(), z.object({ issuer: z.string().url(), subject: z.string().min(1), role }).strict()]);
}
const accountPolicy = z.object({
  members: z.array(declaredMemberSchema(accountRoleSchema)).max(1000).optional(),
  directoryGroupMappings: z.array(z.object({ issuer: z.string().url(), group: id, role: accountRoleSchema }).strict()).max(1000).optional(),
}).strict();
export const universeAccessPolicySchema = z.object({
  members: z.array(declaredMemberSchema(universeRole)).max(1000).optional(),
  directoryGroupMappings: z.array(z.object({ issuer: z.string().url(), group: id, role: universeRole }).strict()).max(1000).optional(),
}).strict();
export const accountViewSchema = z.object({ id, name, slug: id, kind: z.enum(["personal", "shared", "development"]), enabled: z.boolean(), role: accountRoleSchema.nullable(), universeQuota: z.number().int().nonnegative(), defaultProtected: z.boolean(), requireProtected: z.boolean(), membershipPolicy: accountPolicy.optional(), createdAt: timestamp });
export const accountCreateSchema = z.object({ name, slug: handle, kind: z.enum(["shared", "development"]).default("shared"), initialAdminUserId: id.optional(), universeQuota: z.number().int().min(0).max(100000).optional(), defaultProtected: z.boolean().optional(), requireProtected: z.boolean().optional() }).strict();
export const accountUpdateSchema = z.object({ name: name.optional(), enabled: z.boolean().optional(), universeQuota: z.number().int().min(0).max(100000).optional(), defaultProtected: z.boolean().optional(), requireProtected: z.boolean().optional(), membershipPolicy: accountPolicy.optional() }).strict();
export const accountMemberPutSchema = z.object({ userId: id, role: accountRoleSchema.default("member") }).strict();
export const accountMemberViewSchema = z.object({ id, userId: id, name: z.string(), email: z.string(), role: accountRoleSchema, createdAt: timestamp });
export const applicationViewSchema = z.object({ id, accountId: id, name, enabled: z.boolean(), role: accountRoleSchema, createdAt: timestamp });
export const applicationCreateSchema = z.object({ name, role: accountRoleSchema.default("member") }).strict();
export const applicationUpdateSchema = z.object({ name: name.optional(), enabled: z.boolean().optional(), role: accountRoleSchema.optional() }).strict();
export const universeGrantPutSchema = z.object({ principal: principalRefSchema, role: universeRole }).strict();
export const universeGrantViewSchema = z.object({ id, universeId: id, principal: principalRefSchema, role: universeRole, name: z.string(), createdAt: timestamp });
export const accountUniverseCreateSchema = z.object({
  universeId: z.uuid().optional(), name, handle: handle.optional(), initialOwner: principalRefSchema.optional(),
  metadata: z.record(z.string(), z.string()).optional(),
  protected: z.boolean().optional(), expiresAtMs: z.number().int().safe().nonnegative().nullable().optional(),
}).strict();
export const accountUniverseAdoptSchema = z.object({ lightspeedUniverseId: z.uuid(), name, initialOwner: principalRefSchema }).strict();
export const universeLifecycleUpdateSchema = z.object({ name: name.optional(), status: z.enum(["active", "archived"]).optional(), metadata: z.record(z.string(), z.string()).optional(), protected: z.boolean().optional(), expiresAtMs: z.number().int().safe().nonnegative().nullable().optional() }).strict();
export const platformUniverseViewSchema = z.object({
  id, accountId: id, lightspeedUniverseId: id, name, handle: id,
  role: universeRole, accountRole: accountRoleSchema.nullable(), status: z.enum(["provisioning", "active", "archived", "deleting"]),
  protected: z.boolean(), expiresAtMs: z.number().nullable(), createdBy: z.string().nullable(),
  features: z.record(z.string(), z.boolean()), icon: z.string(), iconColor: z.string(), gatewayUrl: z.string().nullable(),
  runtimeConfiguration: z.record(z.string(), z.unknown()), createdAt: timestamp, updatedAt: timestamp,
});
const ok = z.object({ ok: z.literal(true) });
const accountPath = { accountId: id };
const universePath = { universe: id };

/** Platform owns identity schemas; the shared runtime catalog remains Rust-owned.
 * Every native client and route generator reads this one descriptor collection. */
export const PLATFORM_OPERATIONS = {
  ...SESSION_OPERATIONS,
  ...CREDENTIAL_OPERATIONS,
  "platform/accounts/list": { helper: { name: "listAccounts" }, method: "GET", path: "/api/v1/platform/accounts", params: z.object({}).strict(), result: z.object({ accounts: z.array(accountViewSchema), canCreate: z.boolean() }) },
  "platform/accounts/create": { helper: { name: "createAccount" }, method: "POST", path: "/api/v1/platform/accounts", params: accountCreateSchema, result: z.object({ account: accountViewSchema }) },
  "platform/accounts/read": { helper: { name: "readAccount" }, method: "GET", path: "/api/v1/platform/accounts/{accountId}", params: z.object(accountPath).strict(), result: z.object({ account: accountViewSchema }) },
  "platform/accounts/update": { helper: { name: "updateAccount" }, method: "PATCH", path: "/api/v1/platform/accounts/{accountId}", params: accountUpdateSchema.extend(accountPath), result: z.object({ account: accountViewSchema }) },
  "platform/accounts/members/list": { helper: { name: "listAccountMembers" }, method: "GET", path: "/api/v1/platform/accounts/{accountId}/members", params: z.object(accountPath).strict(), result: z.object({ members: z.array(accountMemberViewSchema) }) },
  "platform/accounts/members/put": { helper: { name: "putAccountMember" }, method: "PUT", path: "/api/v1/platform/accounts/{accountId}/members/{userId}", params: accountMemberPutSchema.extend(accountPath), result: z.object({ member: accountMemberViewSchema }) },
  "platform/accounts/members/delete": { helper: { name: "removeAccountMember" }, method: "DELETE", path: "/api/v1/platform/accounts/{accountId}/members/{userId}", params: z.object({ ...accountPath, userId: id }).strict(), result: ok },
  "platform/accounts/applications/list": { helper: { name: "listApplications" }, method: "GET", path: "/api/v1/platform/accounts/{accountId}/applications", params: z.object(accountPath).strict(), result: z.object({ applications: z.array(applicationViewSchema) }) },
  "platform/accounts/applications/create": { helper: { name: "createApplication" }, method: "POST", path: "/api/v1/platform/accounts/{accountId}/applications", params: applicationCreateSchema.extend(accountPath), result: z.object({ application: applicationViewSchema }) },
  "platform/accounts/applications/update": { helper: { name: "updateApplication" }, method: "PATCH", path: "/api/v1/platform/accounts/{accountId}/applications/{applicationId}", params: applicationUpdateSchema.extend({ ...accountPath, applicationId: id }), result: z.object({ application: applicationViewSchema }) },
  "platform/accounts/applications/delete": { helper: { name: "disableApplication" }, method: "DELETE", path: "/api/v1/platform/accounts/{accountId}/applications/{applicationId}", params: z.object({ ...accountPath, applicationId: id }).strict(), result: ok },
  "platform/universes/adopt": { helper: { name: "adoptUniverse" }, method: "POST", path: "/api/v1/platform/accounts/{accountId}/universes/adopt", params: accountUniverseAdoptSchema.extend(accountPath), result: z.object({ universe: platformUniverseViewSchema }) },
  "platform/universes/create": { helper: { name: "createUniverse" }, method: "POST", path: "/api/v1/platform/accounts/{accountId}/universes", params: accountUniverseCreateSchema.extend(accountPath), result: z.object({ universe: platformUniverseViewSchema }) },
  "platform/universes/update": { helper: { name: "updateUniverse" }, method: "PATCH", path: "/api/v1/platform/universes/{universe}", params: universeLifecycleUpdateSchema.extend(universePath), result: z.object({ universe: platformUniverseViewSchema }) },
  "platform/universes/grants/list": { helper: { name: "listUniverseGrants" }, method: "GET", path: "/api/v1/platform/universes/{universe}/grants", params: z.object(universePath).strict(), result: z.object({ grants: z.array(universeGrantViewSchema) }) },
  "platform/universes/grants/put": { helper: { name: "putUniverseGrant" }, method: "PUT", path: "/api/v1/platform/universes/{universe}/grants", params: universeGrantPutSchema.extend(universePath), result: z.object({ grant: universeGrantViewSchema }) },
  "platform/universes/grants/delete": { helper: { name: "removeUniverseGrant" }, method: "DELETE", path: "/api/v1/platform/universes/{universe}/grants/{grantId}", params: z.object({ ...universePath, grantId: id }).strict(), result: ok },
  "platform/universes/access-policy/put": { helper: { name: "putUniverseAccessPolicy" }, method: "PUT", path: "/api/v1/platform/universes/{universe}/access-policy", params: z.object({ ...universePath, policy: universeAccessPolicySchema }).strict(), result: z.object({ policy: universeAccessPolicySchema }) },
} as const;
export type PlatformOperationId = keyof typeof PLATFORM_OPERATIONS;
export type PlatformOperationParams<K extends PlatformOperationId> = z.input<typeof PLATFORM_OPERATIONS[K]["params"]>;
export type PlatformOperationResult<K extends PlatformOperationId> = z.infer<typeof PLATFORM_OPERATIONS[K]["result"]>;
