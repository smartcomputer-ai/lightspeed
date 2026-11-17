import { z } from "zod";
import { CREDENTIAL_OPERATIONS, credentialRestrictionsSchema } from "./credential-api.js";
import { SESSION_OPERATIONS, platformSessionViewSchema } from "./session-api.js";
import { baseUniverseRoleSchema, organizationActionSchema, organizationRoleSchema, universeRoleSchema } from "./access.js";

/** Platform's extension to common connection discovery; core owns the common fields. */
export const platformConnectionDetailsSchema = z.object({
  principal: platformSessionViewSchema.shape.principal.omit({ email: true }).extend({
    platformAdmin: z.boolean().describe("Whether the principal is a Platform Admin. Credential restrictions still apply to every operation."),
  }),
  credential: platformSessionViewSchema.shape.credential,
  organizations: z.array(z.object({ id: z.string(), name: z.string(), role: organizationRoleSchema })),
  restrictions: credentialRestrictionsSchema,
});
export type PlatformConnectionDetails = z.infer<typeof platformConnectionDetailsSchema>;
export const principalRefSchema = z.object({ kind: z.enum(["user", "serviceUser"]), id: z.string().min(1) }).strict();
export type PrincipalRef = z.infer<typeof principalRefSchema>;
const universeRole = universeRoleSchema;
const id = z.string().min(1);
const name = z.string().trim().min(1).max(120);
const handle = z.string().regex(/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/);
const timestamp = z.string().datetime();
function declaredMemberSchema<T extends z.ZodType>(role: T) {
  return z.union([z.object({ email: z.email(), role }).strict(), z.object({ issuer: z.string().url(), subject: z.string().min(1), role }).strict()]);
}
const organizationPolicy = z.object({
  members: z.array(declaredMemberSchema(organizationRoleSchema)).max(1000).optional(),
  directoryGroupMappings: z.array(z.object({ issuer: z.string().url(), group: id, role: organizationRoleSchema }).strict()).max(1000).optional(),
}).strict();
export const universeAccessPolicySchema = z.object({
  members: z.array(declaredMemberSchema(universeRole)).max(1000).optional(),
  directoryGroupMappings: z.array(z.object({ issuer: z.string().url(), group: id, role: universeRole }).strict()).max(1000).optional(),
}).strict();
export const organizationViewSchema = z.object({ id, name, slug: id, personal: z.boolean(), role: organizationRoleSchema.nullable().describe("The highest organization role the caller holds, for display."), actions: z.array(organizationActionSchema).describe("What the caller may do in the organization: the union of every role it holds there."), universeQuota: z.number().int().nonnegative(), universeCount: z.number().int().nonnegative().optional(), baseUniverseRole: baseUniverseRoleSchema.nullable().describe("The universe role every member person holds on every universe in the organization; null gives members access only through grants."), defaultProtected: z.boolean(), requireProtected: z.boolean(), membershipPolicy: organizationPolicy.optional(), createdAt: timestamp });
export const organizationCreateSchema = z.object({ name, slug: handle, initialAdminUserId: id.optional(), universeQuota: z.number().int().min(0).max(100000).optional(), baseUniverseRole: baseUniverseRoleSchema.nullable().optional(), defaultProtected: z.boolean().optional(), requireProtected: z.boolean().optional() }).strict();
export const organizationUpdateSchema = z.object({ name: name.optional(), universeQuota: z.number().int().min(0).max(100000).optional(), baseUniverseRole: baseUniverseRoleSchema.nullable().optional(), defaultProtected: z.boolean().optional(), requireProtected: z.boolean().optional(), membershipPolicy: organizationPolicy.optional() }).strict();
export const organizationMemberPutSchema = z.object({ userId: id, role: organizationRoleSchema.default("member") }).strict();
export const organizationMemberViewSchema = z.object({ id, userId: id, name: z.string(), email: z.string(), role: organizationRoleSchema, added: z.boolean().describe("Holds an explicit membership that an admin added; removing the member removes it."), declared: z.boolean().describe("The organization's membership policy or a directory group grants membership; only changing that policy removes it."), createdAt: timestamp });
export const serviceUserViewSchema = z.object({ id, organizationId: id, name, enabled: z.boolean(), role: organizationRoleSchema, createdAt: timestamp });
export const serviceUserCreateSchema = z.object({ name, role: organizationRoleSchema.default("member") }).strict();
export const serviceUserUpdateSchema = z.object({ name: name.optional(), enabled: z.boolean().optional(), role: organizationRoleSchema.optional() }).strict();
export const universeGrantPutSchema = z.object({ principal: principalRefSchema, role: universeRole }).strict();
export const universeGrantViewSchema = z.object({ id, universeId: id, principal: principalRefSchema, role: universeRole, name: z.string(), createdAt: timestamp });
export const organizationUniverseCreateSchema = z.object({
  universeId: z.uuid().optional(), name, handle: handle.optional(), initialOwner: principalRefSchema.optional(),
  metadata: z.record(z.string(), z.string()).optional(),
  protected: z.boolean().optional(), expiresAtMs: z.number().int().safe().nonnegative().nullable().optional(),
}).strict();
export const organizationUniverseAdoptSchema = z.object({ lightspeedUniverseId: z.uuid(), name, initialOwner: principalRefSchema }).strict();
export const universeLifecycleUpdateSchema = z.object({ name: name.optional(), status: z.enum(["active", "archived"]).optional(), metadata: z.record(z.string(), z.string()).optional(), protected: z.boolean().optional(), expiresAtMs: z.number().int().safe().nonnegative().nullable().optional() }).strict();
export const platformUniverseViewSchema = z.object({
  id, organizationId: id, lightspeedUniverseId: id, name, handle: id,
  role: universeRole, organizationRole: organizationRoleSchema.nullable(), status: z.enum(["provisioning", "active", "archived", "deleting"]),
  protected: z.boolean(), expiresAtMs: z.number().nullable(), createdBy: z.string().nullable(),
  features: z.record(z.string(), z.boolean()), icon: z.string(), iconColor: z.string(),
  runtimeConfiguration: z.record(z.string(), z.unknown()), createdAt: timestamp, updatedAt: timestamp,
});
const ok = z.object({ ok: z.literal(true) });
const organizationPath = { organizationId: id };
const universePath = { universe: id };

/** Platform owns identity schemas; the shared runtime catalog remains Rust-owned.
 * Every native client and route generator reads this one descriptor collection. */
export const PLATFORM_OPERATIONS = {
  ...SESSION_OPERATIONS,
  ...CREDENTIAL_OPERATIONS,
  "platform/organizations/list": { helper: { name: "listOrganizations" }, method: "GET", path: "/api/v1/platform/organizations", params: z.object({}).strict(), result: z.object({ organizations: z.array(organizationViewSchema), canCreate: z.boolean() }) },
  "platform/organizations/create": { helper: { name: "createOrganization" }, method: "POST", path: "/api/v1/platform/organizations", params: organizationCreateSchema, result: z.object({ organization: organizationViewSchema }) },
  "platform/organizations/read": { helper: { name: "readOrganization" }, method: "GET", path: "/api/v1/platform/organizations/{organizationId}", params: z.object(organizationPath).strict(), result: z.object({ organization: organizationViewSchema }) },
  "platform/organizations/update": { helper: { name: "updateOrganization" }, method: "PATCH", path: "/api/v1/platform/organizations/{organizationId}", params: organizationUpdateSchema.extend(organizationPath), result: z.object({ organization: organizationViewSchema }) },
  "platform/organizations/members/list": { helper: { name: "listOrganizationMembers" }, method: "GET", path: "/api/v1/platform/organizations/{organizationId}/members", params: z.object(organizationPath).strict(), result: z.object({ members: z.array(organizationMemberViewSchema) }) },
  "platform/organizations/members/put": { helper: { name: "putOrganizationMember" }, method: "PUT", path: "/api/v1/platform/organizations/{organizationId}/members/{userId}", params: organizationMemberPutSchema.extend(organizationPath), result: z.object({ member: organizationMemberViewSchema }) },
  "platform/organizations/members/delete": { helper: { name: "removeOrganizationMember" }, method: "DELETE", path: "/api/v1/platform/organizations/{organizationId}/members/{userId}", params: z.object({ ...organizationPath, userId: id }).strict(), result: ok },
  "platform/organizations/service-users/list": { helper: { name: "listServiceUsers" }, method: "GET", path: "/api/v1/platform/organizations/{organizationId}/service-users", params: z.object(organizationPath).strict(), result: z.object({ serviceUsers: z.array(serviceUserViewSchema) }) },
  "platform/organizations/service-users/create": { helper: { name: "createServiceUser" }, method: "POST", path: "/api/v1/platform/organizations/{organizationId}/service-users", params: serviceUserCreateSchema.extend(organizationPath), result: z.object({ serviceUser: serviceUserViewSchema }) },
  "platform/organizations/service-users/update": { helper: { name: "updateServiceUser" }, method: "PATCH", path: "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}", params: serviceUserUpdateSchema.extend({ ...organizationPath, serviceUserId: id }), result: z.object({ serviceUser: serviceUserViewSchema }) },
  "platform/organizations/service-users/delete": { helper: { name: "disableServiceUser" }, method: "DELETE", path: "/api/v1/platform/organizations/{organizationId}/service-users/{serviceUserId}", params: z.object({ ...organizationPath, serviceUserId: id }).strict(), result: ok },
  "platform/universes/adopt": { helper: { name: "adoptUniverse" }, method: "POST", path: "/api/v1/platform/organizations/{organizationId}/universes/adopt", params: organizationUniverseAdoptSchema.extend(organizationPath), result: z.object({ universe: platformUniverseViewSchema }) },
  "platform/universes/create": { helper: { name: "createUniverse" }, method: "POST", path: "/api/v1/platform/organizations/{organizationId}/universes", params: organizationUniverseCreateSchema.extend(organizationPath), result: z.object({ universe: platformUniverseViewSchema }) },
  "platform/universes/update": { helper: { name: "updateUniverse" }, method: "PATCH", path: "/api/v1/platform/universes/{universe}", params: universeLifecycleUpdateSchema.extend(universePath), result: z.object({ universe: platformUniverseViewSchema }) },
  "platform/universes/grants/list": { helper: { name: "listUniverseGrants" }, method: "GET", path: "/api/v1/platform/universes/{universe}/grants", params: z.object(universePath).strict(), result: z.object({ grants: z.array(universeGrantViewSchema) }) },
  "platform/universes/grants/put": { helper: { name: "putUniverseGrant" }, method: "PUT", path: "/api/v1/platform/universes/{universe}/grants", params: universeGrantPutSchema.extend(universePath), result: z.object({ grant: universeGrantViewSchema }) },
  "platform/universes/grants/delete": { helper: { name: "removeUniverseGrant" }, method: "DELETE", path: "/api/v1/platform/universes/{universe}/grants/{grantId}", params: z.object({ ...universePath, grantId: id }).strict(), result: ok },
  "platform/universes/access-policy/put": { helper: { name: "putUniverseAccessPolicy" }, method: "PUT", path: "/api/v1/platform/universes/{universe}/access-policy", params: z.object({ ...universePath, policy: universeAccessPolicySchema }).strict(), result: z.object({ policy: universeAccessPolicySchema }) },
} as const;
export type PlatformOperationId = keyof typeof PLATFORM_OPERATIONS;
export type PlatformOperationParams<K extends PlatformOperationId> = z.input<typeof PLATFORM_OPERATIONS[K]["params"]>;
export type PlatformOperationResult<K extends PlatformOperationId> = z.infer<typeof PLATFORM_OPERATIONS[K]["result"]>;
