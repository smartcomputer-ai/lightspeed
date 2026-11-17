ALTER TABLE "applications" RENAME TO "service_users";--> statement-breakpoint
ALTER TABLE "credential_bindings" RENAME COLUMN "application_id" TO "service_user_id";--> statement-breakpoint
ALTER TABLE "universe_grants" RENAME COLUMN "application_id" TO "service_user_id";--> statement-breakpoint
ALTER TABLE "credential_bindings" DROP CONSTRAINT "credential_bindings_one_principal";--> statement-breakpoint
ALTER TABLE "universe_grants" DROP CONSTRAINT "universe_grants_one_principal";--> statement-breakpoint
ALTER TABLE "service_users" DROP CONSTRAINT "applications_organization_id_organization_id_fk";
--> statement-breakpoint
ALTER TABLE "credential_bindings" DROP CONSTRAINT "credential_bindings_application_id_applications_id_fk";
--> statement-breakpoint
ALTER TABLE "universe_grants" DROP CONSTRAINT "universe_grants_application_id_applications_id_fk";
--> statement-breakpoint
DROP INDEX "applications_organization_idx";--> statement-breakpoint
DROP INDEX "credential_bindings_application_idx";--> statement-breakpoint
DROP INDEX "universe_grants_application_idx";--> statement-breakpoint
ALTER TABLE "service_users" ADD CONSTRAINT "service_users_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "public"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "credential_bindings" ADD CONSTRAINT "credential_bindings_service_user_id_service_users_id_fk" FOREIGN KEY ("service_user_id") REFERENCES "public"."service_users"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "universe_grants" ADD CONSTRAINT "universe_grants_service_user_id_service_users_id_fk" FOREIGN KEY ("service_user_id") REFERENCES "public"."service_users"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "service_users_organization_idx" ON "service_users" USING btree ("organization_id");--> statement-breakpoint
CREATE INDEX "credential_bindings_service_user_idx" ON "credential_bindings" USING btree ("service_user_id");--> statement-breakpoint
CREATE UNIQUE INDEX "universe_grants_service_user_idx" ON "universe_grants" USING btree ("universe_id","service_user_id");--> statement-breakpoint
ALTER TABLE "credential_bindings" ADD CONSTRAINT "credential_bindings_one_principal" CHECK (("credential_bindings"."user_id" IS NOT NULL) <> ("credential_bindings"."service_user_id" IS NOT NULL));--> statement-breakpoint
ALTER TABLE "universe_grants" ADD CONSTRAINT "universe_grants_one_principal" CHECK (("universe_grants"."user_id" IS NOT NULL) <> ("universe_grants"."service_user_id" IS NOT NULL));