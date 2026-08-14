ALTER TABLE "foundry_packs" ADD COLUMN "manager_profile_id" text;
--> statement-breakpoint
UPDATE "foundry_packs"
SET "manager_profile_id" = 'foundry-manager'
WHERE "manager_profile_id" IS NULL;
--> statement-breakpoint
ALTER TABLE "foundry_packs" ALTER COLUMN "manager_profile_id" SET NOT NULL;
--> statement-breakpoint
ALTER TABLE "foundry_packs" ADD COLUMN "runtime_target" jsonb;
--> statement-breakpoint
CREATE TABLE "foundry_releases" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"pack_id" uuid NOT NULL,
	"invocation_id" text NOT NULL,
	"source_commit" text NOT NULL,
	"artifact_digest" text NOT NULL,
	"target" text NOT NULL,
	"outcome" text NOT NULL,
	"initiated_by" text,
	"smoke_passed" boolean,
	"details_ref" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "foundry_releases_invocation_id_unique" UNIQUE("invocation_id")
);
--> statement-breakpoint
ALTER TABLE "foundry_releases" ADD CONSTRAINT "foundry_releases_pack_id_foundry_packs_id_fk" FOREIGN KEY ("pack_id") REFERENCES "public"."foundry_packs"("id") ON DELETE cascade ON UPDATE no action;
--> statement-breakpoint
CREATE INDEX "foundry_releases_pack_idx" ON "foundry_releases" USING btree ("pack_id","created_at");

