-- Generic auth substrate: encrypted secrets, grants, OAuth clients/flows, and
-- provider configuration (P69 G1-G5).
--
-- Design notes:
-- - auth_secrets stores AEAD-encrypted values only. Plaintext never touches
--   Postgres; encryption happens in the store adapter with a configured
--   master key (KMS envelope encryption can replace key_id = 'local-v1'
--   later without a schema change).
-- - auth_grants stores grant lifecycle metadata and secret references, never
--   token values.
-- - auth_clients stores OAuth authorization/token endpoint metadata. The
--   client secret, when present, lives in auth_secrets and is referenced by id.
-- - auth_flows stores one-time authorization-code flows. The OAuth `state`
--   parameter is never stored; only its SHA-256 hash is, and the PKCE
--   verifier lives in auth_secrets.
-- - auth_providers stores non-secret provider configuration with optional
--   credential references into auth_secrets.
-- - All tables are universe-scoped, matching mcp_servers.

CREATE TABLE IF NOT EXISTS auth_secrets (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    secret_id text NOT NULL,
    secret_kind text NOT NULL,
    key_id text NOT NULL,
    nonce bytea NOT NULL,
    ciphertext bytea NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, secret_id),

    CONSTRAINT auth_secrets_secret_id_format
        CHECK (secret_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT auth_secrets_secret_kind_not_empty
        CHECK (secret_kind <> ''),
    CONSTRAINT auth_secrets_key_id_not_empty
        CHECK (key_id <> ''),
    CONSTRAINT auth_secrets_nonce_len
        CHECK (octet_length(nonce) = 12),
    CONSTRAINT auth_secrets_ciphertext_not_empty
        CHECK (octet_length(ciphertext) > 0),
    CONSTRAINT auth_secrets_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT auth_secrets_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT auth_secrets_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE TABLE IF NOT EXISTS auth_grants (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    grant_id text NOT NULL,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    principal_kind text NOT NULL DEFAULT 'universe_default',
    principal_id text,
    display_name text,
    subject_hint text,
    scopes text[] NOT NULL DEFAULT '{}',
    audience text,
    access_token_secret_id text,
    refresh_token_secret_id text,
    oauth_client_id text,
    expires_at_ms bigint,
    status text NOT NULL DEFAULT 'active',
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, grant_id),

    CONSTRAINT auth_grants_grant_id_format
        CHECK (grant_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT auth_grants_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT auth_grants_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_bearer',
                'mcp_oauth',
                'github_app',
                'github_app_user',
                'github_oauth_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth'
            )
        ),
    CONSTRAINT auth_grants_principal_kind_known
        CHECK (principal_kind IN ('user', 'service_account', 'universe_default')),
    CONSTRAINT auth_grants_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT auth_grants_subject_hint_not_empty
        CHECK (subject_hint IS NULL OR subject_hint <> ''),
    CONSTRAINT auth_grants_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT auth_grants_status_known
        CHECK (status IN ('active', 'needs_reauth', 'revoked', 'failed')),
    CONSTRAINT auth_grants_metadata_is_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT auth_grants_expires_at_ms_nonnegative
        CHECK (expires_at_ms IS NULL OR expires_at_ms >= 0),
    CONSTRAINT auth_grants_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT auth_grants_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT auth_grants_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS auth_grants_status_idx
    ON auth_grants (universe_id, status, grant_id);

CREATE INDEX IF NOT EXISTS auth_grants_provider_idx
    ON auth_grants (universe_id, provider_id, grant_id);

CREATE TABLE IF NOT EXISTS auth_clients (
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

    PRIMARY KEY (universe_id, client_id),

    CONSTRAINT auth_clients_client_id_format
        CHECK (client_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT auth_clients_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT auth_clients_provider_kind_oauth
        CHECK (
            provider_kind IN (
                'mcp_oauth',
                'github_app_user',
                'github_oauth_app',
                'custom_oauth'
            )
        ),
    CONSTRAINT auth_clients_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT auth_clients_authorization_endpoint_not_empty
        CHECK (authorization_endpoint <> ''),
    CONSTRAINT auth_clients_token_endpoint_not_empty
        CHECK (token_endpoint <> ''),
    CONSTRAINT auth_clients_remote_client_id_not_empty
        CHECK (remote_client_id <> ''),
    CONSTRAINT auth_clients_auth_method_known
        CHECK (
            token_endpoint_auth_method IN (
                'client_secret_basic',
                'client_secret_post',
                'none'
            )
        ),
    CONSTRAINT auth_clients_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT auth_clients_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT auth_clients_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT auth_clients_updated_after_created
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

-- Generic auth provider configurations. One table serves all provider kinds:
-- GitHub Apps first, future providers add a config variant, not a table.
CREATE TABLE IF NOT EXISTS auth_providers (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    display_name text,
    config_json jsonb NOT NULL DEFAULT '{}',
    credential_secret_id text,
    status text NOT NULL DEFAULT 'active',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, provider_id),

    -- The provider's credential secret cannot be deleted while the provider
    -- references it.
    CONSTRAINT auth_providers_credential_secret_fk
        FOREIGN KEY (universe_id, credential_secret_id)
        REFERENCES auth_secrets (universe_id, secret_id)
        ON DELETE RESTRICT,

    CONSTRAINT auth_providers_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT auth_providers_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_bearer',
                'mcp_oauth',
                'github_app',
                'github_app_user',
                'github_oauth_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth'
            )
        ),
    CONSTRAINT auth_providers_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT auth_providers_status_known
        CHECK (status IN ('active', 'needs_configuration', 'disabled')),
    CONSTRAINT auth_providers_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT auth_providers_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT auth_providers_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

COMMENT ON TABLE auth_secrets IS
    'Universe-scoped AEAD-encrypted auth secret values; plaintext exists only in adapter memory.';
COMMENT ON COLUMN auth_secrets.key_id IS
    'Identifies the key that sealed this row (local master key or KMS envelope key) for rotation.';
COMMENT ON TABLE auth_grants IS
    'Universe-scoped auth grants referencing auth_secrets; never stores token values.';
COMMENT ON COLUMN auth_grants.audience IS
    'Normalized resource the grant is bound to (for MCP: the server resource URL). NULL means unrestricted.';
COMMENT ON COLUMN auth_grants.oauth_client_id IS
    'OAuth client configuration the grant was minted through; used for refresh.';
COMMENT ON TABLE auth_clients IS
    'Universe-scoped OAuth client configurations; secrets live in auth_secrets.';
COMMENT ON TABLE auth_flows IS
    'One-time authorization-code flows; stores the state hash, never the state or tokens.';

-- The scripts re-run idempotently at startup, so widening a kind list must
-- also swap the constraint on databases created before the new kind existed
-- (P69 G6 added 'model_api_key'). DROP + ADD as a pair stays idempotent.
ALTER TABLE auth_grants DROP CONSTRAINT IF EXISTS auth_grants_provider_kind_known;
ALTER TABLE auth_grants ADD CONSTRAINT auth_grants_provider_kind_known
    CHECK (
        provider_kind IN (
            'static_bearer',
            'mcp_oauth',
            'github_app',
            'github_app_user',
            'github_oauth_app',
            'custom_oauth',
            'model_api_key',
            'model_oauth'
        )
    );
ALTER TABLE auth_providers DROP CONSTRAINT IF EXISTS auth_providers_provider_kind_known;
ALTER TABLE auth_providers ADD CONSTRAINT auth_providers_provider_kind_known
    CHECK (
        provider_kind IN (
            'static_bearer',
            'mcp_oauth',
            'github_app',
            'github_app_user',
            'github_oauth_app',
            'custom_oauth',
            'model_api_key',
            'model_oauth'
        )
    );
