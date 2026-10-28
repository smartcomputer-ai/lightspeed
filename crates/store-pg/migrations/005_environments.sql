-- Environment compute: deployment providers, universe bindings, registration
-- keys and their secrets, and universe environments of three kinds.
--
-- Design notes:
-- - A provider is reached at one validated controller endpoint URL (ws://,
--   wss:// or unix://). Its bearer credential is optional; when present it is
--   sealed with AES-256-GCM by the store adapter, exactly like auth_secrets.
--   data_route_base is optional operator configuration; without it the data
--   routes derive from the controller endpoint.
-- - A binding is a universe's use of one provider, keyed by the provider.
-- - A registration key is an admission policy and the group of the
--   environments it admitted. It stores no counters: registration and active
--   counts derive from environment rows carrying registration_key_id. Only
--   SHA-256 hashes of its server-generated secrets are stored, in a child
--   table, so a rotation keeps the group.
-- - One environments row covers three kinds. Lightspeed owns a five-state
--   lifecycle; the observed_* columns and public_endpoint are what the
--   provider last reported, for display only. A registered row's identity
--   mode is read from its key.
-- - The daemon public key is the identity. One public key maps to at most
--   one environment in the whole deployment, ever: the unique index covers
--   closed rows too, so a closed environment's daemon cannot register again
--   without a fresh local identity.

CREATE TABLE IF NOT EXISTS environment_providers (
    provider_id text PRIMARY KEY,
    display_name text,
    controller_endpoint text NOT NULL,
    data_route_base text,
    credential_key_id text,
    credential_nonce bytea,
    credential_ciphertext bytea,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    CONSTRAINT environment_providers_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_providers_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT environment_providers_controller_endpoint_format CHECK (
        controller_endpoint ~ '^(wss?|unix)://[^[:space:][:cntrl:]]+$'
        AND octet_length(controller_endpoint) <= 2048
    ),
    CONSTRAINT environment_providers_data_route_base_format CHECK (
        data_route_base IS NULL
        OR (
            data_route_base ~ '^wss?://[^[:space:][:cntrl:]]+$'
            AND octet_length(data_route_base) <= 2048
        )
    ),
    CONSTRAINT environment_providers_credential_all_or_none CHECK (
        (credential_key_id IS NULL AND credential_nonce IS NULL AND credential_ciphertext IS NULL)
        OR (
            credential_key_id IS NOT NULL AND credential_key_id <> ''
            AND credential_nonce IS NOT NULL AND octet_length(credential_nonce) = 12
            AND credential_ciphertext IS NOT NULL
        )
    ),
    CONSTRAINT environment_providers_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

CREATE TABLE IF NOT EXISTS environment_provider_bindings (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    provider_id text NOT NULL REFERENCES environment_providers (provider_id) ON DELETE RESTRICT,
    enabled boolean NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, provider_id),
    CONSTRAINT environment_provider_bindings_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

CREATE TABLE IF NOT EXISTS environment_registration_keys (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    registration_key_id text NOT NULL,
    display_name text NOT NULL,
    identity_mode text NOT NULL,
    max_active_environments integer,
    ephemeral_disconnect_grace_ms bigint,
    offline_close_after_ms bigint,
    expires_at_ms bigint,
    revoked_at_ms bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, registration_key_id),
    CONSTRAINT environment_registration_keys_id_format
        CHECK (registration_key_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_registration_keys_display_name_bounded
        CHECK (display_name <> '' AND length(display_name) <= 128),
    CONSTRAINT environment_registration_keys_identity_mode_known
        CHECK (identity_mode IN ('persistent', 'ephemeral')),
    CONSTRAINT environment_registration_keys_limits_positive CHECK (
        (max_active_environments IS NULL OR max_active_environments > 0)
        AND (ephemeral_disconnect_grace_ms IS NULL OR ephemeral_disconnect_grace_ms > 0)
        AND (offline_close_after_ms IS NULL OR offline_close_after_ms > 0)
    ),
    CONSTRAINT environment_registration_keys_offline_close_persistent
        CHECK (offline_close_after_ms IS NULL OR identity_mode = 'persistent'),
    CONSTRAINT environment_registration_keys_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
        AND (expires_at_ms IS NULL OR expires_at_ms >= 0)
        AND (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms)
    )
);

-- Secrets of a key: rotation retires one and adds another, so the key's
-- group, allowlists and identity mode survive. secret_hash is SHA-256 of the
-- server-generated secret; it is unique deployment-wide, retired secrets
-- included.
CREATE TABLE IF NOT EXISTS environment_registration_key_secrets (
    secret_hash text PRIMARY KEY,
    universe_id uuid NOT NULL,
    registration_key_id text NOT NULL,
    key_prefix text NOT NULL,
    created_at_ms bigint NOT NULL,
    retired_at_ms bigint,
    FOREIGN KEY (universe_id, registration_key_id)
        REFERENCES environment_registration_keys (universe_id, registration_key_id)
        ON DELETE CASCADE,
    CONSTRAINT environment_registration_key_secrets_hash_format
        CHECK (secret_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT environment_registration_key_secrets_prefix_not_empty
        CHECK (key_prefix <> ''),
    CONSTRAINT environment_registration_key_secrets_times_valid CHECK (
        created_at_ms >= 0 AND (retired_at_ms IS NULL OR retired_at_ms >= created_at_ms)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS environment_registration_key_secrets_live_idx
    ON environment_registration_key_secrets (universe_id, registration_key_id)
    WHERE retired_at_ms IS NULL;

CREATE TABLE IF NOT EXISTS environments (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    environment_id text NOT NULL,
    source_kind text NOT NULL,
    state text NOT NULL,
    state_message text,
    close_reason text,
    state_changed_at_ms bigint NOT NULL,
    request_id text,
    display_name text,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    daemon_version text,
    daemon_git_sha text,
    daemon_protocol_version integer,
    -- provisioned (no FK on provider_id: closed rows outlive providers)
    provider_id text,
    template_id text,
    create_idle_json jsonb,
    provider_target_id text,
    next_attempt_at_ms bigint,
    attempts integer NOT NULL DEFAULT 0,
    observed_phase text,
    observed_power text,
    observed_message text,
    observed_at_ms bigint,
    public_endpoint text,
    -- external
    daemon_endpoint text,
    -- registered
    registration_key_id text,
    daemon_public_key text,
    connected boolean,
    connection_changed_at_ms bigint,
    last_seen_at_ms bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    closed_at_ms bigint,
    PRIMARY KEY (universe_id, environment_id),
    -- Deferrable so a universe delete can check at commit, after its
    -- cascade has removed the environments too.
    CONSTRAINT environments_registration_key_fk
        FOREIGN KEY (universe_id, registration_key_id)
        REFERENCES environment_registration_keys (universe_id, registration_key_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT environments_ids_format CHECK (
        environment_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        AND (request_id IS NULL OR request_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$')
        AND (template_id IS NULL OR template_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$')
        AND (
            provider_target_id IS NULL
            OR (
                octet_length(provider_target_id) BETWEEN 1 AND 255
                AND provider_target_id !~ '[[:cntrl:]]'
            )
        )
    ),
    CONSTRAINT environments_source_known
        CHECK (source_kind IN ('provisioned', 'external', 'registered')),
    CONSTRAINT environments_state_known
        CHECK (state IN ('provisioning', 'open', 'closing', 'closed', 'failed')),
    CONSTRAINT environments_state_per_kind
        CHECK (source_kind = 'provisioned' OR state IN ('open', 'closed')),
    CONSTRAINT environments_close_reason CHECK (
        (close_reason IS NULL) = (state NOT IN ('closing', 'closed'))
        AND (
            close_reason IS NULL
            OR close_reason IN (
                'requested', 'key_revoked', 'ephemeral_grace',
                'departed', 'offline_timeout', 'provider_ended'
            )
        )
    ),
    CONSTRAINT environments_closed_at
        CHECK ((closed_at_ms IS NULL) = (state <> 'closed')),
    CONSTRAINT environments_state_message_bounded CHECK (
        state_message IS NULL OR octet_length(state_message) BETWEEN 1 AND 1024
    ),
    CONSTRAINT environments_work_schedule CHECK (
        (next_attempt_at_ms IS NOT NULL)
            = (source_kind = 'provisioned' AND state IN ('provisioning', 'closing'))
        AND attempts >= 0
    ),
    CONSTRAINT environments_daemon_build CHECK (
        (daemon_protocol_version IS NOT NULL OR (daemon_version IS NULL AND daemon_git_sha IS NULL))
        AND (daemon_version IS NULL OR daemon_version <> '')
        AND (daemon_git_sha IS NULL OR daemon_git_sha <> '')
    ),
    CONSTRAINT environments_source_fields CHECK (
        (
            source_kind = 'provisioned'
            AND request_id IS NOT NULL AND provider_id IS NOT NULL AND template_id IS NOT NULL
            AND daemon_endpoint IS NULL
            AND registration_key_id IS NULL AND daemon_public_key IS NULL
            AND connected IS NULL AND connection_changed_at_ms IS NULL AND last_seen_at_ms IS NULL
        )
        OR (
            source_kind = 'external'
            AND request_id IS NOT NULL
            AND provider_id IS NULL AND template_id IS NULL AND create_idle_json IS NULL
            AND provider_target_id IS NULL
            AND observed_phase IS NULL AND public_endpoint IS NULL
            AND daemon_endpoint IS NOT NULL
            AND daemon_endpoint ~ '^(wss?|unix)://[^[:space:][:cntrl:]]+$'
            AND octet_length(daemon_endpoint) <= 2048
            AND registration_key_id IS NULL AND daemon_public_key IS NULL
            AND connected IS NULL AND connection_changed_at_ms IS NULL AND last_seen_at_ms IS NULL
        )
        OR (
            source_kind = 'registered'
            AND request_id IS NULL
            AND provider_id IS NULL AND template_id IS NULL AND create_idle_json IS NULL
            AND provider_target_id IS NULL
            AND observed_phase IS NULL AND public_endpoint IS NULL
            AND daemon_endpoint IS NULL
            AND registration_key_id IS NOT NULL
            AND daemon_public_key IS NOT NULL AND daemon_public_key ~ '^[0-9a-f]{64}$'
            AND connected IS NOT NULL
            AND connection_changed_at_ms IS NOT NULL AND connection_changed_at_ms >= 0
            AND last_seen_at_ms IS NOT NULL AND last_seen_at_ms >= 0
        )
    ),
    CONSTRAINT environments_observation CHECK (
        (observed_phase IS NULL) = (observed_at_ms IS NULL)
        AND (
            observed_phase IS NULL
            OR observed_phase IN ('provisioning', 'active', 'closing', 'closed', 'failed', 'unknown')
        )
        AND (
            observed_power IS NULL
            OR (
                observed_phase IS NOT NULL AND observed_phase = 'active'
                AND observed_power IN ('running', 'paused', 'suspended', 'stopped')
            )
        )
        AND (
            observed_message IS NULL
            OR (observed_phase IS NOT NULL AND octet_length(observed_message) BETWEEN 1 AND 1024)
        )
        AND (
            public_endpoint IS NULL
            OR (public_endpoint <> '' AND octet_length(public_endpoint) <= 2048 AND state <> 'closed')
        )
    ),
    CONSTRAINT environments_json_objects CHECK (
        jsonb_typeof(metadata_json) = 'object'
        AND (create_idle_json IS NULL OR jsonb_typeof(create_idle_json) = 'object')
    ),
    CONSTRAINT environments_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
        AND state_changed_at_ms >= created_at_ms
        AND (closed_at_ms IS NULL OR closed_at_ms >= created_at_ms)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS environments_request_id_idx
    ON environments (universe_id, request_id)
    WHERE request_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS environments_provider_state_idx
    ON environments (universe_id, provider_id, state)
    WHERE provider_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS environments_registration_key_state_idx
    ON environments (universe_id, registration_key_id, state)
    WHERE registration_key_id IS NOT NULL;

-- The reconciler's work queue (provisioning and closing provisioned rows).
CREATE INDEX IF NOT EXISTS environments_work_idx
    ON environments (next_attempt_at_ms)
    WHERE next_attempt_at_ms IS NOT NULL;

-- Deployment-wide: one daemon identity, one environment, ever (closed rows included).
CREATE UNIQUE INDEX IF NOT EXISTS environments_daemon_public_key_idx
    ON environments (daemon_public_key)
    WHERE daemon_public_key IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS environments_provider_target_idx
    ON environments (provider_id, provider_target_id)
    WHERE provider_target_id IS NOT NULL AND state <> 'closed';

CREATE INDEX IF NOT EXISTS environments_registered_open_idx
    ON environments (universe_id, connected, connection_changed_at_ms)
    WHERE source_kind = 'registered' AND state = 'open';

-- The display sweep's scan set.
CREATE INDEX IF NOT EXISTS environments_provisioned_open_idx
    ON environments (universe_id)
    WHERE source_kind = 'provisioned' AND state = 'open';

CREATE INDEX IF NOT EXISTS environments_metadata_idx
    ON environments USING gin (metadata_json jsonb_path_ops);

CREATE TABLE IF NOT EXISTS environment_credentials (
    universe_id uuid NOT NULL,
    environment_id text NOT NULL,
    env_name text NOT NULL,
    source_kind text NOT NULL,
    grant_id text,
    auth_provider_id text,
    secret_id text,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, environment_id, env_name),
    FOREIGN KEY (universe_id, environment_id)
        REFERENCES environments (universe_id, environment_id) ON DELETE CASCADE,
    -- Deferrable so a universe delete can check at commit, after its
    -- cascade has removed the bindings too.
    FOREIGN KEY (universe_id, grant_id)
        REFERENCES auth_grants (universe_id, grant_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    FOREIGN KEY (universe_id, auth_provider_id)
        REFERENCES auth_providers (universe_id, provider_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    FOREIGN KEY (universe_id, secret_id)
        REFERENCES auth_secrets (universe_id, secret_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT environment_credentials_env_name_format
        CHECK (env_name ~ '^[A-Za-z_][A-Za-z0-9_]{0,127}$'),
    CONSTRAINT environment_credentials_source_kind_known
        CHECK (source_kind IN ('auth_grant', 'auth_provider_credential', 'direct_secret')),
    CONSTRAINT environment_credentials_source_exactly_one CHECK (
        (source_kind = 'auth_grant' AND grant_id IS NOT NULL AND auth_provider_id IS NULL AND secret_id IS NULL)
        OR (source_kind = 'auth_provider_credential' AND grant_id IS NULL AND auth_provider_id IS NOT NULL AND secret_id IS NULL)
        OR (source_kind = 'direct_secret' AND grant_id IS NULL AND auth_provider_id IS NULL AND secret_id IS NOT NULL)
    ),
    CONSTRAINT environment_credentials_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

COMMENT ON TABLE environment_providers IS
    'Operator-registered provider identity, controller endpoint, optional data route base and optional sealed credential.';
COMMENT ON COLUMN environment_providers.data_route_base IS
    'Operator-configured ws:// or wss:// base of the provider data routes; null derives it from the controller endpoint.';
COMMENT ON COLUMN environment_providers.credential_ciphertext IS
    'AES-256-GCM sealed bearer credential for the controller and data routes, AAD = provider id; all three credential columns are null without a credential.';
COMMENT ON TABLE environment_provider_bindings IS
    'A universe''s use of one provider; enabled admits creates, template listing and data routes.';
COMMENT ON TABLE environment_registration_keys IS
    'Reusable universe-scoped admission policies for outbound envd registration; each key is also the group of the environments it admitted.';
COMMENT ON TABLE environment_registration_key_secrets IS
    'SHA-256 hashes of a registration key''s secrets; exactly one is live, rotation retires it and adds another.';
COMMENT ON TABLE environments IS
    'Universe-owned environments of three kinds (provisioned, external, registered) with the five-state lifecycle Lightspeed owns.';
COMMENT ON COLUMN environments.create_idle_json IS
    'Create-time idle policy passed to target/create on every retry; never interpreted by Lightspeed.';
COMMENT ON COLUMN environments.daemon_public_key IS
    'Lowercase hex Ed25519 public key of the registered daemon; the identity, unique across the deployment including closed rows.';
COMMENT ON COLUMN environments.last_seen_at_ms IS
    'Coarse registered liveness: written at connect, at disconnect, and at most every 5 minutes while connected.';
COMMENT ON TABLE environment_credentials IS
    'Universe-owned credential bindings for an environment.';
