ALTER TABLE "organization" ADD COLUMN "base_universe_role" text DEFAULT 'viewer';--> statement-breakpoint
UPDATE "organization" SET "base_universe_role" = NULL WHERE "personal_user_id" IS NOT NULL;
