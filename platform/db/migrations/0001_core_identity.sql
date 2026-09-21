-- Greenfield identity cutover: old authorization must never be inferred or imported.
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM "user") OR EXISTS (SELECT 1 FROM universes) THEN
    RAISE EXCEPTION 'Identity cutover requires a fresh Platform database; provision canonical principals and re-create Platform accounts/linkages explicitly';
  END IF;
END $$;
--> statement-breakpoint
ALTER TABLE "invitation" DISABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "member" DISABLE ROW LEVEL SECURITY;--> statement-breakpoint
ALTER TABLE "organization" DISABLE ROW LEVEL SECURITY;--> statement-breakpoint
DROP TABLE "invitation" CASCADE;--> statement-breakpoint
DROP TABLE "member" CASCADE;--> statement-breakpoint
DROP TABLE "organization" CASCADE;--> statement-breakpoint
ALTER TABLE "universes" DROP CONSTRAINT "universes_organization_id_unique";--> statement-breakpoint
ALTER TABLE "universes" DROP CONSTRAINT IF EXISTS "universes_organization_id_organization_id_fk";
--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "core_principal_id" text NOT NULL;--> statement-breakpoint
ALTER TABLE "universes" ADD COLUMN "slug" text NOT NULL;--> statement-breakpoint
ALTER TABLE "session" DROP COLUMN "active_organization_id";--> statement-breakpoint
ALTER TABLE "session" DROP COLUMN "impersonated_by";--> statement-breakpoint
ALTER TABLE "user" DROP COLUMN "role";--> statement-breakpoint
ALTER TABLE "user" DROP COLUMN "banned";--> statement-breakpoint
ALTER TABLE "user" DROP COLUMN "ban_reason";--> statement-breakpoint
ALTER TABLE "user" DROP COLUMN "ban_expires";--> statement-breakpoint
ALTER TABLE "universes" DROP COLUMN "organization_id";--> statement-breakpoint
ALTER TABLE "user" ADD CONSTRAINT "user_core_principal_id_unique" UNIQUE("core_principal_id");--> statement-breakpoint
ALTER TABLE "universes" ADD CONSTRAINT "universes_slug_unique" UNIQUE("slug");