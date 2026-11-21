CREATE SCHEMA "artifacts";
--> statement-breakpoint
CREATE TABLE "artifacts"."publications" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"organization_id" text NOT NULL,
	"universe_id" uuid NOT NULL,
	"title" text NOT NULL,
	"workspace_id" text NOT NULL,
	"path" text NOT NULL,
	"blob_ref" text NOT NULL,
	"media_type" text,
	"size_bytes" integer NOT NULL,
	"content" "bytea" NOT NULL,
	"created_by" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "publications_content_size" CHECK ("artifacts"."publications"."size_bytes" = octet_length("artifacts"."publications"."content") AND "artifacts"."publications"."size_bytes" <= 1048576)
);
--> statement-breakpoint
ALTER TABLE "artifacts"."publications" ADD CONSTRAINT "publications_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "public"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "artifacts"."publications" ADD CONSTRAINT "publications_universe_id_universes_id_fk" FOREIGN KEY ("universe_id") REFERENCES "public"."universes"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "publications_scope_created" ON "artifacts"."publications" USING btree ("organization_id","universe_id","created_at","id");