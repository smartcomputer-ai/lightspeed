CREATE TABLE "foundry_jobs" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"pack_id" uuid NOT NULL,
	"request" text NOT NULL,
	"status" text DEFAULT 'pending' NOT NULL,
	"submission_id" text NOT NULL,
	"run_id" text,
	"failure" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "foundry_jobs_submission_id_unique" UNIQUE("submission_id")
);
--> statement-breakpoint
CREATE TABLE "foundry_packs" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"universe_id" uuid NOT NULL,
	"name" text NOT NULL,
	"kind" text DEFAULT 'workflow' NOT NULL,
	"repo_url" text NOT NULL,
	"environment_id" text,
	"enabled" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "foundry_reports" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"job_id" uuid NOT NULL,
	"pack_id" uuid NOT NULL,
	"invocation_id" text NOT NULL,
	"phase" text NOT NULL,
	"summary" text NOT NULL,
	"payload" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "foundry_reports_invocation_id_unique" UNIQUE("invocation_id")
);
--> statement-breakpoint
ALTER TABLE "foundry_jobs" ADD CONSTRAINT "foundry_jobs_pack_id_foundry_packs_id_fk" FOREIGN KEY ("pack_id") REFERENCES "public"."foundry_packs"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "foundry_packs" ADD CONSTRAINT "foundry_packs_universe_id_universes_id_fk" FOREIGN KEY ("universe_id") REFERENCES "public"."universes"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "foundry_reports" ADD CONSTRAINT "foundry_reports_job_id_foundry_jobs_id_fk" FOREIGN KEY ("job_id") REFERENCES "public"."foundry_jobs"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "foundry_reports" ADD CONSTRAINT "foundry_reports_pack_id_foundry_packs_id_fk" FOREIGN KEY ("pack_id") REFERENCES "public"."foundry_packs"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "foundry_jobs_pack_idx" ON "foundry_jobs" USING btree ("pack_id","created_at");--> statement-breakpoint
CREATE UNIQUE INDEX "foundry_packs_universe_name_idx" ON "foundry_packs" USING btree ("universe_id","name");--> statement-breakpoint
CREATE INDEX "foundry_reports_job_idx" ON "foundry_reports" USING btree ("job_id","created_at");
