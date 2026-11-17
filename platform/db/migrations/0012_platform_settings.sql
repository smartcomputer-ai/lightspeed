CREATE TABLE "platform_settings" (
	"key" text PRIMARY KEY NOT NULL,
	"value" jsonb NOT NULL,
	"updated_by" text,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "organization" ALTER COLUMN "universe_quota" DROP DEFAULT;--> statement-breakpoint
ALTER TABLE "organization" ALTER COLUMN "universe_quota" DROP NOT NULL;--> statement-breakpoint
-- Limits that were copied from the built-in defaults now follow the deployment default.
UPDATE "organization" SET "universe_quota" = NULL WHERE ("personal_user_id" IS NOT NULL AND "universe_quota" = 25) OR ("personal_user_id" IS NULL AND "universe_quota" = 100);
