CREATE TABLE "organization_modules" (
	"organization_id" text NOT NULL,
	"module_id" text NOT NULL,
	"enabled" boolean NOT NULL,
	CONSTRAINT "organization_modules_organization_id_module_id_pk" PRIMARY KEY("organization_id","module_id")
);
--> statement-breakpoint
ALTER TABLE "organization_modules" ADD CONSTRAINT "organization_modules_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "public"."organization"("id") ON DELETE cascade ON UPDATE no action;