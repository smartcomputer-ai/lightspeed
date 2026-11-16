CREATE TABLE "apikey" (
	"id" text PRIMARY KEY NOT NULL,
	"config_id" text DEFAULT 'default' NOT NULL,
	"name" text,
	"start" text,
	"reference_id" text NOT NULL,
	"prefix" text,
	"key" text NOT NULL,
	"refill_interval" integer,
	"refill_amount" integer,
	"last_refill_at" timestamp with time zone,
	"enabled" boolean DEFAULT true NOT NULL,
	"rate_limit_enabled" boolean DEFAULT true NOT NULL,
	"rate_limit_time_window" integer DEFAULT 86400000,
	"rate_limit_max" integer DEFAULT 10,
	"request_count" integer DEFAULT 0 NOT NULL,
	"remaining" integer,
	"last_request" timestamp with time zone,
	"expires_at" timestamp with time zone,
	"created_at" timestamp with time zone NOT NULL,
	"updated_at" timestamp with time zone NOT NULL,
	"permissions" text,
	"metadata" text
);
--> statement-breakpoint
CREATE TABLE "applications" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"organization_id" text NOT NULL,
	"name" text NOT NULL,
	"enabled" boolean DEFAULT true NOT NULL,
	"role" text DEFAULT 'member' NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "credential_bindings" (
	"key_id" text PRIMARY KEY NOT NULL,
	"user_id" text,
	"application_id" uuid,
	"restrictions" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_by" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "credential_bindings_one_principal" CHECK (("credential_bindings"."user_id" IS NOT NULL) <> ("credential_bindings"."application_id" IS NOT NULL))
);
--> statement-breakpoint
CREATE TABLE "universe_grants" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"universe_id" uuid NOT NULL,
	"user_id" text,
	"application_id" uuid,
	"role" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "universe_grants_one_principal" CHECK (("universe_grants"."user_id" IS NOT NULL) <> ("universe_grants"."application_id" IS NOT NULL))
);
--> statement-breakpoint
ALTER TABLE "universes" DROP CONSTRAINT "universes_organization_id_unique";--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "kind" text DEFAULT 'shared' NOT NULL;--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "enabled" boolean DEFAULT true NOT NULL;--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "personal_user_id" text;--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "universe_quota" integer DEFAULT 100 NOT NULL;--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "default_protected" boolean DEFAULT true NOT NULL;--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "require_protected" boolean DEFAULT false NOT NULL;--> statement-breakpoint
ALTER TABLE "organization" ADD COLUMN "membership_policy" jsonb DEFAULT '{}'::jsonb NOT NULL;--> statement-breakpoint
ALTER TABLE "universes" ADD COLUMN "handle" text NOT NULL;--> statement-breakpoint
ALTER TABLE "universes" ADD COLUMN "created_by" text;--> statement-breakpoint
ALTER TABLE "universes" ADD COLUMN "access_policy" jsonb DEFAULT '{}'::jsonb NOT NULL;--> statement-breakpoint
ALTER TABLE "applications" ADD CONSTRAINT "applications_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "public"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "credential_bindings" ADD CONSTRAINT "credential_bindings_key_id_apikey_id_fk" FOREIGN KEY ("key_id") REFERENCES "public"."apikey"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "credential_bindings" ADD CONSTRAINT "credential_bindings_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "credential_bindings" ADD CONSTRAINT "credential_bindings_application_id_applications_id_fk" FOREIGN KEY ("application_id") REFERENCES "public"."applications"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "universe_grants" ADD CONSTRAINT "universe_grants_universe_id_universes_id_fk" FOREIGN KEY ("universe_id") REFERENCES "public"."universes"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "universe_grants" ADD CONSTRAINT "universe_grants_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "universe_grants" ADD CONSTRAINT "universe_grants_application_id_applications_id_fk" FOREIGN KEY ("application_id") REFERENCES "public"."applications"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "apikey_config_idx" ON "apikey" USING btree ("config_id");--> statement-breakpoint
CREATE INDEX "apikey_reference_idx" ON "apikey" USING btree ("reference_id");--> statement-breakpoint
CREATE UNIQUE INDEX "apikey_key_idx" ON "apikey" USING btree ("key");--> statement-breakpoint
CREATE INDEX "applications_organization_idx" ON "applications" USING btree ("organization_id");--> statement-breakpoint
CREATE INDEX "credential_bindings_user_idx" ON "credential_bindings" USING btree ("user_id");--> statement-breakpoint
CREATE INDEX "credential_bindings_application_idx" ON "credential_bindings" USING btree ("application_id");--> statement-breakpoint
CREATE UNIQUE INDEX "universe_grants_user_idx" ON "universe_grants" USING btree ("universe_id","user_id");--> statement-breakpoint
CREATE UNIQUE INDEX "universe_grants_application_idx" ON "universe_grants" USING btree ("universe_id","application_id");--> statement-breakpoint
ALTER TABLE "organization" ADD CONSTRAINT "organization_personal_user_id_user_id_fk" FOREIGN KEY ("personal_user_id") REFERENCES "public"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
CREATE UNIQUE INDEX "member_organization_user_idx" ON "member" USING btree ("organization_id","user_id");--> statement-breakpoint
ALTER TABLE "organization" ADD CONSTRAINT "organization_personal_user_id_unique" UNIQUE("personal_user_id");--> statement-breakpoint
ALTER TABLE "universes" ADD CONSTRAINT "universes_handle_unique" UNIQUE("handle");