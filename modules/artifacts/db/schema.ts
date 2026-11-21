import { sql } from "drizzle-orm";
import { check, customType, index, integer, pgSchema, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { organization, universes } from "@lightspeed-ai/platform-db/schema";
import { ARTIFACT_MAX_BYTES } from "../contract.js";

const bytea = customType<{ data: Uint8Array; driverData: Buffer }>({
  dataType: () => "bytea",
  toDriver: value => Buffer.from(value),
});

export const artifactsSchema = pgSchema("artifacts");

/** A publication pins the exact file bytes even after its source workspace changes. */
export const publications = artifactsSchema.table("publications", {
  id: uuid("id").primaryKey().defaultRandom(),
  organizationId: text("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  universeId: uuid("universe_id").notNull().references(() => universes.id, { onDelete: "cascade" }),
  title: text("title").notNull(),
  workspaceId: text("workspace_id").notNull(),
  path: text("path").notNull(),
  blobRef: text("blob_ref").notNull(),
  mediaType: text("media_type"),
  sizeBytes: integer("size_bytes").notNull(),
  content: bytea("content").notNull(),
  createdBy: text("created_by").notNull(),
  createdAt: timestamp("created_at", { withTimezone: true, mode: "date" }).defaultNow().notNull(),
}, table => [
  index("publications_scope_created").on(table.organizationId, table.universeId, table.createdAt, table.id),
  check("publications_content_size", sql`${table.sizeBytes} = octet_length(${table.content}) AND ${table.sizeBytes} <= ${sql.raw(String(ARTIFACT_MAX_BYTES))}`),
]);
