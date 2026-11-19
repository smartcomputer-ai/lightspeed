CREATE TABLE "federated_credentials" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"service_user_id" uuid NOT NULL,
	"issuer" text NOT NULL,
	"subject" text NOT NULL,
	"service_account_uid" text,
	"restrictions" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"last_used_at" timestamp with time zone
);
--> statement-breakpoint
ALTER TABLE "federated_credentials" ADD CONSTRAINT "federated_credentials_service_user_id_service_users_id_fk" FOREIGN KEY ("service_user_id") REFERENCES "public"."service_users"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE UNIQUE INDEX "federated_credentials_identity_idx" ON "federated_credentials" USING btree ("issuer","subject");--> statement-breakpoint
CREATE INDEX "federated_credentials_service_user_idx" ON "federated_credentials" USING btree ("service_user_id");