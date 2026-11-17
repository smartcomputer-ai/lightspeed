-- Universe-scoped provider credentials, OAuth state, and model defaults.
-- The store adapter seals secret values before writing them to PostgreSQL.

CREATE TABLE credentials_secrets (
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

    CONSTRAINT credentials_secrets_secret_id_format
        CHECK (secret_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT credentials_secrets_secret_kind_not_empty
        CHECK (secret_kind <> ''),
    CONSTRAINT credentials_secrets_key_id_not_empty
        CHECK (key_id <> ''),
    CONSTRAINT credentials_secrets_nonce_len
        CHECK (octet_length(nonce) = 12),
    CONSTRAINT credentials_secrets_ciphertext_not_empty
        CHECK (octet_length(ciphertext) > 0),
    CONSTRAINT credentials_secrets_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT credentials_secrets_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT credentials_secrets_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE TABLE credentials_grants (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    grant_id text NOT NULL,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    created_by jsonb NOT NULL,
    display_name text,
    subject_hint text,
    scopes text[] NOT NULL DEFAULT '{}',
    audience text,
    access_token_secret_id text,
    refresh_token_secret_id text,
    oauth_client_id text,
    expires_at_ms bigint,
    status text NOT NULL DEFAULT 'active',
    -- Creation-time authority boundary. Retrievable grants may be leased to
    -- trusted service workers; brokered grants never expose plaintext.
    exposure text NOT NULL DEFAULT 'brokered',
    last_leased_at_ms bigint,
    lease_count bigint NOT NULL DEFAULT 0,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, grant_id),

    CONSTRAINT credentials_grants_grant_id_format
        CHECK (grant_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT credentials_grants_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT credentials_grants_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_token',
                'mcp_oauth',
                'github_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth'
            )
        ),
    CONSTRAINT credentials_grants_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT credentials_grants_subject_hint_not_empty
        CHECK (subject_hint IS NULL OR subject_hint <> ''),
    CONSTRAINT credentials_grants_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT credentials_grants_status_known
        CHECK (status IN ('active', 'needs_reauth', 'revoked', 'failed')),
    CONSTRAINT credentials_grants_exposure_known
        CHECK (exposure IN ('brokered', 'retrievable')),
    CONSTRAINT credentials_grants_last_leased_at_ms_nonnegative
        CHECK (last_leased_at_ms IS NULL OR last_leased_at_ms >= 0),
    CONSTRAINT credentials_grants_lease_count_nonnegative
        CHECK (lease_count >= 0),
    CONSTRAINT credentials_grants_metadata_is_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT credentials_grants_expires_at_ms_nonnegative
        CHECK (expires_at_ms IS NULL OR expires_at_ms >= 0),
    CONSTRAINT credentials_grants_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT credentials_grants_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT credentials_grants_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX credentials_grants_status_idx
    ON credentials_grants (universe_id, status, grant_id);

CREATE INDEX credentials_grants_provider_idx
    ON credentials_grants (universe_id, provider_id, grant_id);

CREATE TABLE credentials_clients (
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
    -- Public discovery facts; credentials remain in encrypted secret storage.
    authorization_server_issuer text,
    authorization_response_iss_parameter_supported boolean NOT NULL DEFAULT false,
    authorization_server_scopes_supported text[] NOT NULL DEFAULT '{}',
    audience text,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, client_id),

    CONSTRAINT credentials_clients_client_id_format
        CHECK (client_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT credentials_clients_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT credentials_clients_provider_kind_oauth
        CHECK (
            provider_kind IN (
                'mcp_oauth',
                'custom_oauth'
            )
        ),
    CONSTRAINT credentials_clients_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT credentials_clients_authorization_endpoint_not_empty
        CHECK (authorization_endpoint <> ''),
    CONSTRAINT credentials_clients_token_endpoint_not_empty
        CHECK (token_endpoint <> ''),
    CONSTRAINT credentials_clients_remote_client_id_not_empty
        CHECK (remote_client_id <> ''),
    CONSTRAINT credentials_clients_auth_method_known
        CHECK (
            token_endpoint_auth_method IN (
                'client_secret_basic',
                'client_secret_post',
                'none'
            )
        ),
    CONSTRAINT credentials_clients_authorization_server_issuer_not_empty
        CHECK (authorization_server_issuer IS NULL OR authorization_server_issuer <> ''),
    CONSTRAINT credentials_clients_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT credentials_clients_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT credentials_clients_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT credentials_clients_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE TABLE credentials_flows (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    flow_id text NOT NULL,
    client_id text NOT NULL,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    created_by jsonb NOT NULL,
    state_hash text NOT NULL,
    expected_issuer text,
    require_issuer boolean NOT NULL DEFAULT false,
    pkce_verifier_secret_id text NOT NULL,
    redirect_uri text NOT NULL,
    scopes text[] NOT NULL DEFAULT '{}',
    audience text,
    grant_exposure text NOT NULL DEFAULT 'brokered',
    grant_id text,
    error text,
    expires_at_ms bigint NOT NULL,
    consumed_at_ms bigint,
    completed_at_ms bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, flow_id),

    CONSTRAINT credentials_flows_flow_id_format
        CHECK (flow_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT credentials_flows_client_id_not_empty
        CHECK (client_id <> ''),
    CONSTRAINT credentials_flows_provider_id_not_empty
        CHECK (provider_id <> ''),
    CONSTRAINT credentials_flows_provider_kind_oauth
        CHECK (
            provider_kind IN (
                'mcp_oauth',
                'custom_oauth'
            )
        ),
    CONSTRAINT credentials_flows_expected_issuer_not_empty
        CHECK (expected_issuer IS NULL OR expected_issuer <> ''),
    CONSTRAINT credentials_flows_state_hash_not_empty
        CHECK (state_hash <> ''),
    CONSTRAINT credentials_flows_pkce_verifier_secret_id_not_empty
        CHECK (pkce_verifier_secret_id <> ''),
    CONSTRAINT credentials_flows_redirect_uri_not_empty
        CHECK (redirect_uri <> ''),
    CONSTRAINT credentials_flows_audience_not_empty
        CHECK (audience IS NULL OR audience <> ''),
    CONSTRAINT credentials_flows_grant_exposure_known
        CHECK (grant_exposure IN ('brokered', 'retrievable')),
    CONSTRAINT credentials_flows_outcome_exclusive
        CHECK (grant_id IS NULL OR error IS NULL),
    CONSTRAINT credentials_flows_outcome_requires_completion
        CHECK (
            (grant_id IS NULL AND error IS NULL)
            OR completed_at_ms IS NOT NULL
        ),
    CONSTRAINT credentials_flows_error_not_empty
        CHECK (error IS NULL OR error <> ''),
    CONSTRAINT credentials_flows_expires_at_ms_nonnegative
        CHECK (expires_at_ms >= 0),
    CONSTRAINT credentials_flows_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT credentials_flows_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0)
);

-- The state hash is the callback lookup key and must be unique per universe.
CREATE UNIQUE INDEX credentials_flows_state_hash_idx
    ON credentials_flows (universe_id, state_hash);

CREATE INDEX credentials_flows_expiry_idx
    ON credentials_flows (universe_id, expires_at_ms);

-- Each provider kind supplies its own configuration document.
CREATE TABLE credentials_providers (
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
    CONSTRAINT credentials_providers_credential_secret_fk
        FOREIGN KEY (universe_id, credential_secret_id)
        REFERENCES credentials_secrets (universe_id, secret_id)
        ON DELETE RESTRICT,

    CONSTRAINT credentials_providers_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT credentials_providers_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_token',
                'mcp_oauth',
                'github_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth',
                'model_endpoint'
            )
        ),
    CONSTRAINT credentials_providers_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT credentials_providers_status_known
        CHECK (status IN ('active', 'needs_configuration', 'disabled')),
    CONSTRAINT credentials_providers_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT credentials_providers_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT credentials_providers_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE TABLE credentials_provider_headers (
    universe_id uuid NOT NULL,
    provider_id text NOT NULL,
    name text NOT NULL,
    literal_value text,
    grant_id text,

    PRIMARY KEY (universe_id, provider_id, name),
    FOREIGN KEY (universe_id, provider_id)
        REFERENCES credentials_providers (universe_id, provider_id) ON DELETE CASCADE,
    CONSTRAINT credentials_provider_headers_grant_fk
        FOREIGN KEY (universe_id, grant_id)
        REFERENCES credentials_grants (universe_id, grant_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT credentials_provider_headers_one_source
        CHECK ((literal_value IS NULL) <> (grant_id IS NULL)),
    CONSTRAINT credentials_provider_headers_name_lowercase
        CHECK (name <> '' AND name = lower(name)),
    -- These names carry a secret by convention, and a plain value is returned
    -- to every reader of the provider.
    CONSTRAINT credentials_provider_headers_secret_names_require_grant
        CHECK (
            name NOT IN ('api-key', 'proxy-authorization', 'x-api-key')
            OR grant_id IS NOT NULL
        )
);

CREATE INDEX credentials_provider_headers_grant_idx
    ON credentials_provider_headers (universe_id, grant_id)
    WHERE grant_id IS NOT NULL;

COMMENT ON TABLE credentials_provider_headers IS
    'Extra request headers sent to a model provider''s endpoint; independent of the provider''s own credential, which owns Authorization.';
COMMENT ON COLUMN credentials_provider_headers.literal_value IS
    'Plain, non-secret header value returned by reads.';
COMMENT ON COLUMN credentials_provider_headers.grant_id IS
    'Universe credential grant whose token is resolved per request and sent raw; the token never enters this table.';

COMMENT ON TABLE credentials_secrets IS
    'Universe-scoped AEAD-encrypted auth secret values; plaintext exists only in adapter memory.';
COMMENT ON COLUMN credentials_secrets.key_id IS
    'Identifies the key that sealed this row (local master key or KMS envelope key) for rotation.';
COMMENT ON TABLE credentials_grants IS
    'Universe-scoped credential grants referencing credentials_secrets; never stores token values.';
COMMENT ON COLUMN credentials_grants.audience IS
    'Normalized resource the grant is bound to (for MCP: the server resource URL). NULL means unrestricted.';
COMMENT ON COLUMN credentials_grants.oauth_client_id IS
    'OAuth client configuration the grant was minted through; used for refresh.';
COMMENT ON COLUMN credentials_grants.exposure IS
    'Creation-time authority, immutable through the service: brokered grants stay inside the token broker; only retrievable grants may be leased to trusted services or injected into environments.';
COMMENT ON COLUMN credentials_grants.last_leased_at_ms IS
    'Most recent successful service lease, for operator audit only.';
COMMENT ON COLUMN credentials_grants.lease_count IS
    'Number of successful service leases, for operator audit only.';
COMMENT ON TABLE credentials_clients IS
    'Universe-scoped OAuth client configurations; secrets live in credentials_secrets.';
COMMENT ON TABLE credentials_flows IS
    'One-time authorization-code flows; stores the state hash, never the state or tokens.';

COMMENT ON COLUMN credentials_clients.authorization_server_issuer IS
    'Public authorization-server issuer selected by rmcp discovery; never a credential.';
COMMENT ON COLUMN credentials_flows.expected_issuer IS
    'Issuer frozen when the authorization request was created for RFC 9207 callback validation.';

-- Defaults are universe policy; provider transport and credentials stay in
-- credentials_providers. Retain a row after clears to preserve its revision.
CREATE TABLE universe_model_defaults (
    universe_id uuid PRIMARY KEY REFERENCES universes (universe_id) ON DELETE CASCADE,
    revision bigint NOT NULL CHECK (revision > 0),
    agent_run jsonb,
    speech_to_text jsonb
);
