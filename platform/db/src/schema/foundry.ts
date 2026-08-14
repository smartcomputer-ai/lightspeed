import { relations } from "drizzle-orm";
import {
  boolean,
  index,
  jsonb,
  pgTable,
  text,
  timestamp,
  uniqueIndex,
  uuid,
} from "drizzle-orm/pg-core";
import { universes } from "./platform.js";

const createdAt = () => timestamp("created_at", { withTimezone: true }).defaultNow().notNull();
const updatedAt = () =>
  timestamp("updated_at", { withTimezone: true })
    .defaultNow()
    .$onUpdate(() => new Date())
    .notNull();

export type FoundryRuntimeTarget = {
  kind: "docker" | "kubernetes" | "ssh" | "external";
  name: string;
  metadata?: Record<string, string>;
};

/** Durable identity and operator-owned configuration for one software system. */
export const foundryPacks = pgTable(
  "foundry_packs",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    universeId: uuid("universe_id")
      .notNull()
      .references(() => universes.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    kind: text("kind", { enum: ["workflow"] }).default("workflow").notNull(),
    repoUrl: text("repo_url").notNull(),
    managerProfileId: text("manager_profile_id").notNull(),
    /** Optional override; otherwise the controller resolves pack/dev tags. */
    environmentId: text("environment_id"),
    runtimeTarget: jsonb("runtime_target").$type<FoundryRuntimeTarget>(),
    enabled: boolean("enabled").default(true).notNull(),
    createdAt: createdAt(),
    updatedAt: updatedAt(),
  },
  (t) => [uniqueIndex("foundry_packs_universe_name_idx").on(t.universeId, t.name)],
);

export type FoundryReleaseOutcome = "succeeded" | "failed" | "rolled_back";

/** Idempotent projection of release facts emitted by the manager session. */
export const foundryReleases = pgTable(
  "foundry_releases",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    packId: uuid("pack_id")
      .notNull()
      .references(() => foundryPacks.id, { onDelete: "cascade" }),
    invocationId: text("invocation_id").notNull().unique(),
    sourceCommit: text("source_commit").notNull(),
    artifactDigest: text("artifact_digest").notNull(),
    target: text("target").notNull(),
    outcome: text("outcome", { enum: ["succeeded", "failed", "rolled_back"] }).notNull(),
    initiatedBy: text("initiated_by"),
    smokePassed: boolean("smoke_passed"),
    detailsRef: text("details_ref"),
    createdAt: createdAt(),
  },
  (t) => [index("foundry_releases_pack_idx").on(t.packId, t.createdAt)],
);

export const foundryPacksRelations = relations(foundryPacks, ({ one, many }) => ({
  universe: one(universes, {
    fields: [foundryPacks.universeId],
    references: [universes.id],
  }),
  releases: many(foundryReleases),
}));

export const foundryReleasesRelations = relations(foundryReleases, ({ one }) => ({
  pack: one(foundryPacks, {
    fields: [foundryReleases.packId],
    references: [foundryPacks.id],
  }),
}));
