CREATE TABLE "identity_audit" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"actor_id" text,
	"action" text NOT NULL,
	"target_id" text,
	"universe_id" text,
	"outcome" text NOT NULL,
	"details" jsonb DEFAULT '{}'::jsonb NOT NULL
);
--> statement-breakpoint
ALTER TABLE "session" ADD COLUMN "access_version" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "identity_source" text DEFAULT 'local' NOT NULL;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "oidc_issuer" text;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "oidc_subject" text;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "company_admitted" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "provider_checked_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "emergency_admin" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "user" ADD COLUMN "access_version" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
CREATE INDEX "identity_audit_created_idx" ON "identity_audit" USING btree ("created_at");--> statement-breakpoint
CREATE UNIQUE INDEX "user_oidc_identity_idx" ON "user" USING btree ("oidc_issuer","oidc_subject");