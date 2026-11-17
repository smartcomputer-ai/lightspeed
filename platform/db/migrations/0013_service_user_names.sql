-- Existing duplicates keep the oldest name; later ones get a numbered suffix.
UPDATE "service_users" SET "name" = "service_users"."name" || ' (' || ranked.position || ')'
FROM (SELECT "id", row_number() OVER (PARTITION BY "organization_id", "name" ORDER BY "created_at", "id") AS position FROM "service_users") ranked
WHERE ranked."id" = "service_users"."id" AND ranked.position > 1;--> statement-breakpoint
CREATE UNIQUE INDEX "service_users_organization_name_idx" ON "service_users" USING btree ("organization_id","name");