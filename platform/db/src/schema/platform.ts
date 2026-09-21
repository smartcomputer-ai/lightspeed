import { relations } from "drizzle-orm";
import { integer, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { user } from "./auth.js";

const createdAt = () => timestamp("created_at", { withTimezone: true }).defaultNow().notNull();
const updatedAt = () =>
  timestamp("updated_at", { withTimezone: true })
    .defaultNow()
    .$onUpdate(() => new Date())
    .notNull();

/// Display/routing metadata only. Universe permissions live in core.
export const universes = pgTable("universes", {
  id: uuid("id").primaryKey().defaultRandom(),
  slug: text("slug").notNull().unique(),
  lightspeedUniverseId: uuid("lightspeed_universe_id").notNull().unique(),
  name: text("name").notNull(),
  /// Gateway RPC endpoint; null = the deployment default from env.
  gatewayUrl: text("gateway_url"),
  status: text("status", { enum: ["active", "archived"] })
    .default("active")
    .notNull(),
  createdAt: createdAt(),
  updatedAt: updatedAt(),
});

export type UniverseSetupState = {
  principalId?: string;
  keyPrefix?: string;
  grantId?: string;
  serverId?: string;
  profileId?: string;
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

export const universesRelations = relations(universes, ({ many }) => ({
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
