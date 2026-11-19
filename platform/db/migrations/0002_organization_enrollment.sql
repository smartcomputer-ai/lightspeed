CREATE TABLE "organization_enrollments" (
	"organization_id" text PRIMARY KEY NOT NULL,
	"issuer" text NOT NULL,
	"subject" text NOT NULL,
	"subject_pattern" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "organization_enrollments" ADD CONSTRAINT "organization_enrollments_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "public"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE UNIQUE INDEX "organization_enrollments_identity_idx" ON "organization_enrollments" USING btree ("issuer","subject");--> statement-breakpoint
CREATE INDEX "organization_enrollments_rule_idx" ON "organization_enrollments" USING btree ("issuer","subject_pattern");