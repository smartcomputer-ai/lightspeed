-- Caller credentials now come from configuration. Preserve historical
-- attribution by the credential's name alone; it conveys no authority.
UPDATE auth_grants SET created_by = jsonb_build_object('kind', 'token', 'name', created_by ->> 'keyId') WHERE created_by ->> 'kind' = 'key';
UPDATE auth_flows SET created_by = jsonb_build_object('kind', 'token', 'name', created_by ->> 'keyId') WHERE created_by ->> 'kind' = 'key';
UPDATE sessions SET created_by = jsonb_build_object('kind', 'token', 'name', created_by ->> 'keyId') WHERE created_by ->> 'kind' = 'key';
UPDATE bots SET created_by = jsonb_build_object('kind', 'token', 'name', created_by ->> 'keyId') WHERE created_by ->> 'kind' = 'key';
UPDATE universes_deleted SET requested_by = jsonb_build_object('kind', 'token', 'name', requested_by ->> 'keyId') WHERE requested_by ->> 'kind' = 'key';
UPDATE universes SET deletion_json = jsonb_set(deletion_json, '{deletion,requestedBy}', jsonb_build_object('kind', 'token', 'name', deletion_json #>> '{deletion,requestedBy,keyId}')) WHERE deletion_json #>> '{deletion,requestedBy,kind}' = 'key';

-- Rewrite only attribution locations, never arbitrary user JSON that happens
-- to contain a kind/keyId pair. Session events are the durable authority.
CREATE TEMPORARY TABLE runtime_token_event_attributions (
    event_kind text PRIMARY KEY,
    attribution_path text[] NOT NULL
) ON COMMIT DROP;
INSERT INTO runtime_token_event_attributions (event_kind, attribution_path) VALUES
    ('lightspeed.core.run.accepted', ARRAY['event', 'payload', 'run', 'accepted', 'requested_by']),
    ('lightspeed.core.run.steering_accepted', ARRAY['event', 'payload', 'run', 'steering_accepted', 'requested_by']),
    ('lightspeed.core.run.cancellation_requested', ARRAY['event', 'payload', 'run', 'cancellation_requested', 'requested_by']),
    ('lightspeed.core.run.queued_cancelled', ARRAY['event', 'payload', 'run', 'queued_cancelled', 'requested_by']),
    ('lightspeed.core.approval.decided', ARRAY['event', 'payload', 'approval', 'decided', 'decided_by']);

-- Checkpoints may contain attribution inherited from a source session too.
-- They are disposable pointers; replay rebuilds them from the updated log.
DELETE FROM session_checkpoints;
UPDATE session_events target
SET entry_json = jsonb_set(target.entry_json, attributed.attribution_path,
    jsonb_build_object('kind', 'token', 'name', target.entry_json #>> array_append(attributed.attribution_path, 'keyId')))
FROM runtime_token_event_attributions attributed
WHERE target.event_kind = attributed.event_kind
  AND target.entry_json #>> array_append(attributed.attribution_path, 'kind') = 'key';

DROP TABLE api_keys;
