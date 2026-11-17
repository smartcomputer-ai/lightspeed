import { relations, sql } from "drizzle-orm";
import { boolean, check, index, integer, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { apikey, organization, user } from "./auth.js";

const createdAt = () => timestamp("created_at", { withTimezone: true }).defaultNow().notNull();
const updatedAt = () =>
  timestamp("updated_at", { withTimezone: true })
    .defaultNow()
    .$onUpdate(() => new Date())
    .notNull();

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

/// A runtime universe belongs to one account; creator identity is attribution only.
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

/** Independently enabled automation, owned by an account rather than a human profile. */
export const applications = pgTable("applications", {
  id: uuid("id").primaryKey().defaultRandom(),
  organizationId: text("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  name: text("name").notNull(),
  enabled: boolean("enabled").default(true).notNull(),
  role: text("role", { enum: ["admin", "creator", "member"] }).default("member").notNull(),
  createdAt: createdAt(),
  updatedAt: updatedAt(),
}, (table) => [index("applications_organization_idx").on(table.organizationId)]);

/** Membership in the containing account remains mandatory for every grant. */
export const universeGrants = pgTable("universe_grants", {
  id: uuid("id").primaryKey().defaultRandom(),
  universeId: uuid("universe_id").notNull().references(() => universes.id, { onDelete: "cascade" }),
  userId: text("user_id").references(() => user.id, { onDelete: "cascade" }),
  applicationId: uuid("application_id").references(() => applications.id, { onDelete: "cascade" }),
  role: text("role", { enum: ["owner", "operator", "contributor", "viewer"] }).notNull(),
  createdAt: createdAt(),
  updatedAt: updatedAt(),
}, (table) => [
  check("universe_grants_one_principal", sql`(${table.userId} IS NOT NULL) <> (${table.applicationId} IS NOT NULL)`),
  uniqueIndex("universe_grants_user_idx").on(table.universeId, table.userId),
  uniqueIndex("universe_grants_application_idx").on(table.universeId, table.applicationId),
]);

export interface CredentialRestrictions {
  accountIds?: string[];
  universeIds?: string[];
  operations?: string[];
  readOnly?: boolean;
  allowUniverseCreation?: boolean;
}

/** Protected association; caller-controlled Better Auth metadata never selects identity. */
export const credentialBindings = pgTable("credential_bindings", {
  keyId: text("key_id").primaryKey().references(() => apikey.id, { onDelete: "cascade" }),
  userId: text("user_id").references(() => user.id, { onDelete: "cascade" }),
  applicationId: uuid("application_id").references(() => applications.id, { onDelete: "cascade" }),
  restrictions: jsonb("restrictions").$type<CredentialRestrictions>().default({}).notNull(),
  createdBy: text("created_by").notNull(),
  createdAt: createdAt(),
}, (table) => [
  check("credential_bindings_one_principal", sql`(${table.userId} IS NOT NULL) <> (${table.applicationId} IS NOT NULL)`),
  index("credential_bindings_user_idx").on(table.userId),
  index("credential_bindings_application_idx").on(table.applicationId),
]);

export type UniverseSetupState = {
  previous?: { applicationId?: string; credentialId?: string; grantId?: string; serverId?: string };
  applicationId?: string;
  credentialId?: string;
  grantId?: string;
  serverId?: string;
};

/// Provenance for platform-managed, multi-resource universe setups. Engine
/// registries remain authoritative; this row only records which resources an
/// installation owns so retries can repair safely without persisting secrets.
export const universeSetupInstallations = pgTable(
  "universe_setup_installations",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    universeId: uuid("universe_id")
      .notNull()
      .references(() => universes.id, { onDelete: "cascade" }),
    setupId: text("setup_id").notNull(),
    installedVersion: integer("installed_version").default(0).notNull(),
    status: text("status", { enum: ["installing", "ready", "failed"] })
      .default("installing")
      .notNull(),
    state: jsonb("state").$type<UniverseSetupState>().default({}).notNull(),
    error: text("error"),
    installedByUserId: text("installed_by_user_id").references(() => user.id, {
      onDelete: "set null",
    }),
    createdAt: createdAt(),
    updatedAt: updatedAt(),
  },
  (t) => [
    uniqueIndex("universe_setup_installations_universe_setup_idx").on(
      t.universeId,
      t.setupId,
    ),
  ],
);

export const universesRelations = relations(universes, ({ one, many }) => ({
  organization: one(organization, {
    fields: [universes.organizationId],
    references: [organization.id],
  }),
  setupInstallations: many(universeSetupInstallations),
}));

export const universeSetupInstallationsRelations = relations(
  universeSetupInstallations,
  ({ one }) => ({
    universe: one(universes, {
      fields: [universeSetupInstallations.universeId],
      references: [universes.id],
    }),
    installedBy: one(user, {
      fields: [universeSetupInstallations.installedByUserId],
      references: [user.id],
    }),
  }),
);
