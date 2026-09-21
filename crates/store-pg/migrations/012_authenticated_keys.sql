-- Replace attribution-only credentials with canonical scoped authentication.
DROP TABLE api_keys;
CREATE TABLE api_keys (
    key_hash text PRIMARY KEY CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    key_prefix text NOT NULL UNIQUE CHECK (key_prefix <> ''),
    universe_id uuid REFERENCES universes(universe_id) ON DELETE CASCADE,
    principal_id uuid NOT NULL REFERENCES access_principals(principal_id),
    created_by uuid NOT NULL REFERENCES access_principals(principal_id),
    display_name text,
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0),
    revoked_at_ms bigint CHECK (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
    last_used_at_ms bigint
);
CREATE INDEX authenticated_keys_universe_idx ON api_keys(universe_id);

-- Principal attribution is explicit; anonymous records are unsupported.
ALTER TABLE auth_grants ALTER COLUMN principal_kind DROP DEFAULT;
ALTER TABLE auth_flows ALTER COLUMN principal_kind DROP DEFAULT;
ALTER TABLE auth_grants DROP CONSTRAINT auth_grants_principal_kind_known;
ALTER TABLE auth_grants ADD CONSTRAINT auth_grants_principal_kind_known CHECK (principal_kind IN ('user', 'service_account'));
ALTER TABLE auth_flows DROP CONSTRAINT auth_flows_principal_kind_known;
ALTER TABLE auth_flows ADD CONSTRAINT auth_flows_principal_kind_known CHECK (principal_kind IN ('user', 'service_account'));
ALTER TABLE auth_grants ALTER COLUMN principal_id SET NOT NULL;
ALTER TABLE auth_flows ALTER COLUMN principal_id SET NOT NULL;
