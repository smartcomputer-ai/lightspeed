-- Preserve credential ceilings and managed setup references across the terminology change.
UPDATE credential_bindings
SET restrictions = (restrictions - 'accountIds') || jsonb_build_object('organizationIds', restrictions->'accountIds')
WHERE restrictions ? 'accountIds';
--> statement-breakpoint
UPDATE credential_bindings
SET restrictions = jsonb_set(restrictions, '{operations}', (
  SELECT coalesce(jsonb_agg(replace(replace(operation, 'platform/accounts/', 'platform/organizations/'), '/applications/', '/service-users/')), '[]'::jsonb)
  FROM jsonb_array_elements_text(restrictions->'operations') operation
))
WHERE jsonb_typeof(restrictions->'operations') = 'array';
--> statement-breakpoint
UPDATE apikey SET config_id = 'serviceUser' WHERE config_id = 'application';
--> statement-breakpoint
UPDATE universe_setup_installations
SET state = (state - 'applicationId') || jsonb_build_object('serviceUserId', state->'applicationId')
WHERE state ? 'applicationId';
--> statement-breakpoint
UPDATE universe_setup_installations
SET state = jsonb_set(state, '{previous}', ((state->'previous') - 'applicationId') || jsonb_build_object('serviceUserId', state->'previous'->'applicationId'))
WHERE state->'previous' ? 'applicationId';
--> statement-breakpoint
-- This marker prevents sign-in provisioning from restoring a removed development membership.
UPDATE identity_audit SET action = 'organization.development_admission'
WHERE action = 'account.development_admission';
