import { relations, sql } from "drizzle-orm";
import { boolean, check, index, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { apikey, organization, user } from "./auth.js";

const createdAt = () => timestamp("created_at", { withTimezone: true }).defaultNow().notNull();
const updatedAt = () =>
  timestamp("updated_at", { withTimezone: true })
    .defaultNow()
    .$onUpdate(() => new Date())
    .notNull();

/// Deployment-wide settings changed by a Platform Admin. Only values set here
/// are stored; a setting without a row uses its environment value or default.
export const platformSettings = pgTable("platform_settings", {
  key: text("key").primaryKey(),
  value: jsonb("value").notNull(),
  updatedBy: text("updated_by"),
  updatedAt: updatedAt(),
});

// Attribution must survive deletion of the referenced identity or resource.
export const identityAudit = pgTable("identity_audit", {
  id: uuid("id").primaryKey().defaultRandom(),
  createdAt: createdAt(),
  actorId: text("actor_id"),
  action: text("action").notNull(),
  targetId: text("target_id"),
  universeId: text("universe_id"),
  outcome: text("outcome").notNull(),
  details: jsonb("details").$type<Record<string, string | boolean | null>>().default({}).notNull(),
}, (table) => [index("identity_audit_created_idx").on(table.createdAt)]);

/// A runtime universe belongs to one organization; creator identity is attribution only.
export const universes = pgTable("universes", {
  id: uuid("id").primaryKey().defaultRandom(),
  organizationId: text("organization_id")
    .notNull()
    .references(() => organization.id, { onDelete: "cascade" }),
  lightspeedUniverseId: uuid("lightspeed_universe_id").notNull().unique(),
  name: text("name").notNull(),
  handle: text("handle").notNull().unique(),
  createdBy: text("created_by"),
  accessPolicy: jsonb("access_policy").$type<{
    members?: { email?: string; issuer?: string; subject?: string; role: "owner" | "operator" | "contributor" | "viewer" }[];
    directoryGroupMappings?: { issuer: string; group: string; role: "owner" | "operator" | "contributor" | "viewer" }[];
  }>().default({}).notNull(),
  /// Last successful Runtime projection; declarations never replace manual members.
  runtimeConfiguration: jsonb("runtime_configuration").$type<Record<string, unknown>>().default({}).notNull(),
  icon: text("icon").default("orbit").notNull(),
  iconColor: text("icon_color").default("default").notNull(),
  status: text("status", { enum: ["provisioning", "active", "archived", "deleting"] })
    .default("active")
    .notNull(),
  /// Feature switches set away from their default (see the shared feature
  /// registry); an empty map is every feature at its default.
  features: jsonb("features").$type<Record<string, boolean>>().default({}).notNull(),
  createdAt: createdAt(),
  updatedAt: updatedAt(),
});

/** Independently enabled automation, owned by an organization rather than a human profile. */
export const serviceUsers = pgTable("service_users", {
  id: uuid("id").primaryKey().defaultRandom(),
  organizationId: text("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  name: text("name").notNull(),
  enabled: boolean("enabled").default(true).notNull(),
  role: text("role", { enum: ["admin", "creator", "member"] }).default("member").notNull(),
  createdAt: createdAt(),
  updatedAt: updatedAt(),
}, (table) => [
  index("service_users_organization_idx").on(table.organizationId),
  // Documents name service users, so a name is unique in its organization.
  uniqueIndex("service_users_organization_name_idx").on(table.organizationId, table.name),
]);

/** Membership in the containing organization remains mandatory for every grant. */
export const universeGrants = pgTable("universe_grants", {
  id: uuid("id").primaryKey().defaultRandom(),
  universeId: uuid("universe_id").notNull().references(() => universes.id, { onDelete: "cascade" }),
  userId: text("user_id").references(() => user.id, { onDelete: "cascade" }),
  serviceUserId: uuid("service_user_id").references(() => serviceUsers.id, { onDelete: "cascade" }),
  role: text("role", { enum: ["owner", "operator", "contributor", "viewer"] }).notNull(),
  createdAt: createdAt(),
  updatedAt: updatedAt(),
}, (table) => [
  check("universe_grants_one_principal", sql`(${table.userId} IS NOT NULL) <> (${table.serviceUserId} IS NOT NULL)`),
  uniqueIndex("universe_grants_user_idx").on(table.universeId, table.userId),
  uniqueIndex("universe_grants_service_user_idx").on(table.universeId, table.serviceUserId),
]);

export interface CredentialRestrictions {
  organizationIds?: string[];
  universeIds?: string[];
  operations?: string[];
  readOnly?: boolean;
  allowUniverseCreation?: boolean;
  /** Declare service-user keys with supplied secrets; only a human creates keys otherwise. */
  allowServiceUserKeys?: boolean;
}

/** Protected association; caller-controlled Better Auth metadata never selects identity. */
export const credentialBindings = pgTable("credential_bindings", {
  keyId: text("key_id").primaryKey().references(() => apikey.id, { onDelete: "cascade" }),
  userId: text("user_id").references(() => user.id, { onDelete: "cascade" }),
  serviceUserId: uuid("service_user_id").references(() => serviceUsers.id, { onDelete: "cascade" }),
  restrictions: jsonb("restrictions").$type<CredentialRestrictions>().default({}).notNull(),
  createdBy: text("created_by").notNull(),
  createdAt: createdAt(),
}, (table) => [
  check("credential_bindings_one_principal", sql`(${table.userId} IS NOT NULL) <> (${table.serviceUserId} IS NOT NULL)`),
  index("credential_bindings_user_idx").on(table.userId),
  index("credential_bindings_service_user_idx").on(table.serviceUserId),
]);

/** Bind the verified issuer identifier, so reusing a configuration name cannot transfer authority. */
export const federatedCredentials = pgTable("federated_credentials", {
  id: uuid("id").primaryKey().defaultRandom(),
  serviceUserId: uuid("service_user_id").notNull().references(() => serviceUsers.id, { onDelete: "cascade" }),
  issuer: text("issuer").notNull(),
  subject: text("subject").notNull(),
  serviceAccountUid: text("service_account_uid"),
  restrictions: jsonb("restrictions").$type<CredentialRestrictions>().default({}).notNull(),
  createdAt: createdAt(),
  lastUsedAt: timestamp("last_used_at", { withTimezone: true }),
}, table => [
  uniqueIndex("federated_credentials_identity_idx").on(table.issuer, table.subject),
  index("federated_credentials_service_user_idx").on(table.serviceUserId),
]);

export const universesRelations = relations(universes, ({ one }) => ({
  organization: one(organization, {
    fields: [universes.organizationId],
    references: [organization.id],
  }),
}));

