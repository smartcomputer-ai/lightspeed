-- Generic OAuth substrate: client configurations and authorization flows
-- (P69 G2/G3).
--
-- Design notes:
-- - oauth_clients stores manually configured authorization/token endpoint
--   metadata. The client secret, when present, lives in secret_records and
--   is referenced by id; this table never stores secret values.
-- - auth_flows stores one-time authorization-code flows. The OAuth `state`
--   parameter is never stored; only its SHA-256 hash is, and the PKCE
--   verifier lives in secret_records. consumed_at_ms enforces one-time use,
--   completed_at_ms records the terminal outcome.
-- - auth_grants.oauth_client_id links OAuth-minted grants back to their
--   client configuration so the broker can refresh them.

CREATE TABLE IF NOT EXISTS oauth_clients (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    client_id text NOT NULL,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    display_name text,
    authorization_endpoint text NOT NULL,
    token_endpoint text NOT NULL,
    remote_client_id text NOT NULL,
    client_secret_secret_id text,
    token_endpoint_auth_method text NOT NULL DEFAULT 'client_secret_basic',
    scopes_default text[] NOT NULL DEFAULT '{}',
    audience text,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, client_id),

    CONSTRAINT oauth_clients_client_id_format
        CHECK (client_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT oauth_clients_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT oauth_clients_provider_kind_oauth
        CHECK (
            provider_kind IN (
                'mcp_oauth',
                'github_app_user',
                'github_oauth_app',
                'custom_oauth'
            )
        ),
    CONSTRAINT oauth_clients_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT oauth_clients_authorization_endpoint_not_empty
        CHECK (authorization_endpoint <> ''),
    CONSTRAINT oauth_clients_token_endpoint_not_empty
        CHECK (token_endpoint <> ''),
    CONSTRAINT oauth_clients_remote_client_id_not_empty
        CHECK (remote_client_id <> ''),
    CONSTRAINT oauth_clients_auth_method_known
        CHECK (
            token_endpoint_auth_method IN (
                'client_secret_basic',
                'client_secret_post',
                'none'
            )
        ),
    CONSTRAINT oauth_clients_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT oauth_clients_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT oauth_clients_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT oauth_clients_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE TABLE IF NOT EXISTS auth_flows (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    flow_id text NOT NULL,
    client_id text NOT NULL,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    principal_kind text NOT NULL DEFAULT 'universe_default',
    principal_id text,
    state_hash text NOT NULL,
    pkce_verifier_secret_id text NOT NULL,
    redirect_uri text NOT NULL,
    scopes text[] NOT NULL DEFAULT '{}',
    audience text,
    grant_id text,
    error text,
    expires_at_ms bigint NOT NULL,
    consumed_at_ms bigint,
    completed_at_ms bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, flow_id),

    CONSTRAINT auth_flows_flow_id_format
        CHECK (flow_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT auth_flows_client_id_not_empty
        CHECK (client_id <> ''),
    CONSTRAINT auth_flows_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT auth_flows_provider_kind_oauth
        CHECK (
            provider_kind IN (
                'mcp_oauth',
                'github_app_user',
                'github_oauth_app',
                'custom_oauth'
            )
        ),
    CONSTRAINT auth_flows_principal_kind_known
        CHECK (principal_kind IN ('user', 'service_account', 'universe_default')),
    CONSTRAINT auth_flows_state_hash_not_empty
        CHECK (state_hash <> ''),
    CONSTRAINT auth_flows_pkce_verifier_secret_id_not_empty
        CHECK (pkce_verifier_secret_id <> ''),
    CONSTRAINT auth_flows_redirect_uri_not_empty
        CHECK (redirect_uri <> ''),
    CONSTRAINT auth_flows_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT auth_flows_outcome_exclusive
        CHECK (grant_id IS NULL OR error IS NULL),
    CONSTRAINT auth_flows_outcome_requires_completion
        CHECK (
            (grant_id IS NULL AND error IS NULL)
            OR completed_at_ms IS NOT NULL
        ),
    CONSTRAINT auth_flows_error_not_empty
        CHECK (error IS NULL OR error <> ''),
    CONSTRAINT auth_flows_expires_at_ms_nonnegative
        CHECK (expires_at_ms >= 0),
    CONSTRAINT auth_flows_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT auth_flows_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0)
);

-- The state hash is the callback lookup key and must be unique per universe.
CREATE UNIQUE INDEX IF NOT EXISTS auth_flows_state_hash_idx
    ON auth_flows (universe_id, state_hash);

CREATE INDEX IF NOT EXISTS auth_flows_expiry_idx
    ON auth_flows (universe_id, expires_at_ms);

-- Links OAuth-minted grants back to their client configuration for refresh.
ALTER TABLE auth_grants
    ADD COLUMN IF NOT EXISTS oauth_client_id text;

COMMENT ON TABLE oauth_clients IS
    'Universe-scoped OAuth client configurations; secrets live in secret_records.';
COMMENT ON TABLE auth_flows IS
    'One-time authorization-code flows; stores the state hash, never the state or tokens.';
COMMENT ON COLUMN auth_grants.oauth_client_id IS
    'OAuth client configuration the grant was minted through; used for refresh.';
