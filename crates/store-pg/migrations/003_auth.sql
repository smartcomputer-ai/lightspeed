-- Generic auth substrate: encrypted secrets and auth grants (P69 G1).
--
-- Design notes:
-- - secret_records stores AEAD-encrypted values only. Plaintext never touches
--   Postgres; encryption happens in the store adapter with a configured
--   master key (KMS envelope encryption can replace key_id = 'local-v1'
--   later without a schema change).
-- - auth_grants stores grant lifecycle metadata and secret references, never
--   token values. OAuth client/flow/lease tables arrive with P69 G2+.
-- - Both tables are universe-scoped, matching mcp_servers.

CREATE TABLE IF NOT EXISTS secret_records (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    secret_id text NOT NULL,
    secret_kind text NOT NULL,
    key_id text NOT NULL,
    nonce bytea NOT NULL,
    ciphertext bytea NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, secret_id),

    CONSTRAINT secret_records_secret_id_format
        CHECK (secret_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT secret_records_secret_kind_not_empty
        CHECK (secret_kind <> ''),
    CONSTRAINT secret_records_key_id_not_empty
        CHECK (key_id <> ''),
    CONSTRAINT secret_records_nonce_len
        CHECK (octet_length(nonce) = 12),
    CONSTRAINT secret_records_ciphertext_not_empty
        CHECK (octet_length(ciphertext) > 0),
    CONSTRAINT secret_records_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT secret_records_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT secret_records_updated_after_created
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
    expires_at_ms bigint,
    status text NOT NULL DEFAULT 'active',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

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
                'custom_oauth'
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

COMMENT ON TABLE secret_records IS
    'Universe-scoped AEAD-encrypted secret values; plaintext exists only in adapter memory.';
COMMENT ON COLUMN secret_records.key_id IS
    'Identifies the key that sealed this row (local master key or KMS envelope key) for rotation.';
COMMENT ON TABLE auth_grants IS
    'Universe-scoped auth grants referencing secret_records; never stores token values.';
COMMENT ON COLUMN auth_grants.audience IS
    'Normalized resource the grant is bound to (for MCP: the server resource URL). NULL means unrestricted.';
