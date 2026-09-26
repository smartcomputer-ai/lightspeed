-- Environment compute: deployment providers, universe bindings, and
-- durable logical environments with incarnation-scoped physical facts.

CREATE TABLE IF NOT EXISTS environment_providers (
    provider_id text PRIMARY KEY,
    display_name text,
    controller_connection_json jsonb NOT NULL,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    CONSTRAINT environment_providers_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_providers_json_objects CHECK (
        jsonb_typeof(controller_connection_json) = 'object'
        AND jsonb_typeof(metadata_json) = 'object'
    ),
    CONSTRAINT environment_providers_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

CREATE TABLE IF NOT EXISTS environment_provider_bindings (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    binding_id text NOT NULL,
    provider_id text NOT NULL REFERENCES environment_providers (provider_id) ON DELETE RESTRICT,
    status text NOT NULL,
    revision bigint NOT NULL,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, binding_id),
    UNIQUE (universe_id, provider_id),
    CONSTRAINT environment_provider_bindings_binding_id_format
        CHECK (binding_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_provider_bindings_status_known
        CHECK (status IN ('enabled', 'disabled')),
    CONSTRAINT environment_provider_bindings_revision_positive CHECK (revision > 0),
    CONSTRAINT environment_provider_bindings_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT environment_provider_bindings_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

-- Key-based outbound environment registration: reusable, universe-scoped
-- registration keys and the registered environment source they admit.
--
-- Design notes:
-- - A registration key is an admission policy and the group of the
--   environments it admitted. It stores no counters: registration and active
--   counts derive from environment rows carrying registration_key_id.
-- - Only the SHA-256 hash of the server-generated secret is stored, exactly
--   like api_keys. key_prefix is the display handle.
-- - identity_mode is key policy copied onto every environment the key
--   admits; the daemon neither knows nor chooses it.
-- - The daemon public key is the identity. One public key maps to at most
--   one environment in the whole deployment, ever: the unique index covers
--   closed rows too, so a closed environment's daemon cannot register again
--   without a fresh local identity.
-- - last_seen_at_ms is the gateway's heartbeat stamp on the control
--   connection; the lifecycle reconciler derives ephemeral cleanup and stale
--   Ready repair from it.

CREATE TABLE IF NOT EXISTS environment_registration_keys (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    registration_key_id text NOT NULL,
    display_name text NOT NULL,
    key_prefix text NOT NULL,
    secret_hash text NOT NULL,
    identity_mode text NOT NULL,
    max_active_environments integer,
    ephemeral_disconnect_grace_ms bigint,
    expires_at_ms bigint,
    created_at_ms bigint NOT NULL,
    revoked_at_ms bigint,
    PRIMARY KEY (universe_id, registration_key_id),
    CONSTRAINT environment_registration_keys_id_format
        CHECK (registration_key_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_registration_keys_display_name_bounded
        CHECK (display_name <> '' AND length(display_name) <= 128),
    CONSTRAINT environment_registration_keys_key_prefix_not_empty
        CHECK (key_prefix <> ''),
    CONSTRAINT environment_registration_keys_secret_hash_format
        CHECK (secret_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT environment_registration_keys_identity_mode_known
        CHECK (identity_mode IN ('persistent', 'ephemeral')),
    CONSTRAINT environment_registration_keys_limits_positive CHECK (
        (max_active_environments IS NULL OR max_active_environments > 0)
        AND (ephemeral_disconnect_grace_ms IS NULL OR ephemeral_disconnect_grace_ms > 0)
    ),
    CONSTRAINT environment_registration_keys_times_valid CHECK (
        created_at_ms >= 0
        AND (expires_at_ms IS NULL OR expires_at_ms >= 0)
        AND (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS environment_registration_keys_secret_hash_idx
    ON environment_registration_keys (secret_hash);

CREATE UNIQUE INDEX IF NOT EXISTS environment_registration_keys_key_prefix_idx
    ON environment_registration_keys (key_prefix);

CREATE TABLE IF NOT EXISTS environments (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    environment_id text NOT NULL,
    request_id text NOT NULL,
    source_kind text NOT NULL,
    provider_id text,
    binding_id text,
    daemon_connection_json jsonb,
    registration_key_id text,
    daemon_id text,
    daemon_public_key text,
    identity_mode text,
    last_seen_at_ms bigint,
    display_name text,
    status text NOT NULL,
    -- Lightspeed-owned power intent and optional staged idle policy. Observed
    -- power remains in status; activity is read from the daemon on demand.
    desired_power text NOT NULL DEFAULT 'running',
    idle_policy_json jsonb,
    current_incarnation_id text NOT NULL,
    public_ingress_enabled boolean NOT NULL DEFAULT false,
    public_endpoint text,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, environment_id),
    UNIQUE (universe_id, request_id),
    CONSTRAINT environments_registration_key_fk
        FOREIGN KEY (universe_id, registration_key_id)
        REFERENCES environment_registration_keys (universe_id, registration_key_id)
        ON DELETE RESTRICT,
    CONSTRAINT environments_ids_format CHECK (
        environment_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        AND request_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        AND current_incarnation_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
    ),
    CONSTRAINT environments_source_known
        CHECK (source_kind IN ('provisioned', 'external', 'registered')),
    CONSTRAINT environments_source_fields CHECK (
        (
            source_kind = 'provisioned'
            AND provider_id IS NOT NULL AND binding_id IS NOT NULL
            AND daemon_connection_json IS NULL
            AND registration_key_id IS NULL AND daemon_id IS NULL
            AND daemon_public_key IS NULL AND identity_mode IS NULL
            AND last_seen_at_ms IS NULL
        )
        OR (
            source_kind = 'external'
            AND provider_id IS NULL AND binding_id IS NULL
            AND jsonb_typeof(daemon_connection_json) = 'object'
            AND registration_key_id IS NULL AND daemon_id IS NULL
            AND daemon_public_key IS NULL AND identity_mode IS NULL
            AND last_seen_at_ms IS NULL
        )
        OR (
            source_kind = 'registered'
            AND provider_id IS NULL AND binding_id IS NULL
            AND daemon_connection_json IS NULL
            AND registration_key_id IS NOT NULL
            AND daemon_id ~ '^daemon_[0-9a-f]{64}$'
            AND daemon_public_key ~ '^[0-9a-f]{64}$'
            AND identity_mode IN ('persistent', 'ephemeral')
            AND (last_seen_at_ms IS NULL OR last_seen_at_ms >= 0)
        )
    ),
    CONSTRAINT environments_status_known CHECK (status IN (
        'provisioning', 'booting', 'ready', 'paused', 'suspended', 'offline',
        'closing', 'closed', 'failed', 'unknown'
    )),
    CONSTRAINT environments_desired_power_known
        CHECK (desired_power IN ('running', 'paused', 'suspended', 'stopped')),
    CONSTRAINT environments_idle_policy_object
        CHECK (idle_policy_json IS NULL OR jsonb_typeof(idle_policy_json) = 'object'),
    CONSTRAINT environments_power_provisioned_only CHECK (
        source_kind = 'provisioned'
        OR (desired_power = 'running' AND idle_policy_json IS NULL)
    ),
    CONSTRAINT environments_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT environments_public_ingress_fields CHECK (
        (public_ingress_enabled AND source_kind = 'provisioned' AND public_endpoint IS NOT NULL AND public_endpoint <> '')
        OR (NOT public_ingress_enabled AND public_endpoint IS NULL)
    ),
    CONSTRAINT environments_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

CREATE INDEX IF NOT EXISTS environments_binding_status_idx
    ON environments (universe_id, binding_id, status, environment_id);

CREATE INDEX IF NOT EXISTS environments_idle_policy_idx
    ON environments (universe_id)
    WHERE idle_policy_json IS NOT NULL AND status = 'ready';

-- Deployment-wide: one daemon identity, one environment, ever.
CREATE UNIQUE INDEX IF NOT EXISTS environments_daemon_public_key_idx
    ON environments (daemon_public_key)
    WHERE daemon_public_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS environments_registration_key_idx
    ON environments (universe_id, registration_key_id, status)
    WHERE registration_key_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS environments_metadata_idx
    ON environments USING gin (metadata_json jsonb_path_ops);

CREATE TABLE IF NOT EXISTS environment_incarnations (
    universe_id uuid NOT NULL,
    environment_id text NOT NULL,
    incarnation_id text NOT NULL,
    provision_request_id text,
    provider_target_id text,
    template_id text,
    adoption_source_target text,
    power_states_json jsonb NOT NULL DEFAULT '[]',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, environment_id, incarnation_id),
    FOREIGN KEY (universe_id, environment_id)
        REFERENCES environments (universe_id, environment_id) ON DELETE CASCADE,
    UNIQUE (universe_id, provision_request_id),
    CONSTRAINT environment_incarnations_ids_format CHECK (
        incarnation_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        AND (
            provision_request_id IS NULL
            OR provision_request_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        )
        AND (
            template_id IS NULL
            OR template_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        )
        AND (
            adoption_source_target IS NULL
            OR (
                length(adoption_source_target) BETWEEN 1 AND 255
                AND adoption_source_target !~ '[[:cntrl:]]'
            )
        )
    ),
    CONSTRAINT environment_incarnations_source_fields CHECK (
        (
            provision_request_id IS NOT NULL
            AND ((template_id IS NOT NULL) <> (adoption_source_target IS NOT NULL))
        )
        OR (
            provision_request_id IS NULL
            AND provider_target_id IS NULL
            AND template_id IS NULL
            AND adoption_source_target IS NULL
        )
    ),
    CONSTRAINT environment_incarnations_power_states_array
        CHECK (jsonb_typeof(power_states_json) = 'array'),
    CONSTRAINT environment_incarnations_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

-- Incarnations point back to environments; add the other side after both exist.
ALTER TABLE environments
    ADD CONSTRAINT environments_current_incarnation_fk
    FOREIGN KEY (universe_id, environment_id, current_incarnation_id)
    REFERENCES environment_incarnations (universe_id, environment_id, incarnation_id)
    DEFERRABLE INITIALLY DEFERRED;

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
    FOREIGN KEY (universe_id, grant_id)
        REFERENCES auth_grants (universe_id, grant_id) ON DELETE RESTRICT,
    FOREIGN KEY (universe_id, auth_provider_id)
        REFERENCES auth_providers (universe_id, provider_id) ON DELETE RESTRICT,
    FOREIGN KEY (universe_id, secret_id)
        REFERENCES auth_secrets (universe_id, secret_id) ON DELETE RESTRICT,
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
    'Operator-registered provider identity and controller connection; protocol and presence are observed transiently.';
COMMENT ON COLUMN environment_providers.metadata_json IS
    'Non-authoritative operator metadata; never provider capability, health, or allocation policy.';
COMMENT ON TABLE environment_provider_bindings IS
    'Revisioned universe routing and admission binding to one provider; it is not a provider policy document.';
COMMENT ON COLUMN environment_provider_bindings.metadata_json IS
    'Non-authoritative binding labels; never provider template, quota, capacity, or ingress policy.';
COMMENT ON TABLE environments IS
    'Universe-owned logical environments. External environments store a Lightspeed-reachable envd endpoint; provisioned environments retain provider routing linkage.';
COMMENT ON TABLE environment_incarnations IS
    'Lightspeed-authorized environment generations with stable provider retry and target linkage; not provider inventory or live gateway state.';
COMMENT ON COLUMN environment_incarnations.provider_target_id IS
    'Opaque provider-scoped target handle returned by createTarget; interpreted with the environment provider identity.';
COMMENT ON COLUMN environment_incarnations.adoption_source_target IS
    'Provider-native source reference for an explicit operator-managed adoption; retained for idempotent lifecycle retries.';
COMMENT ON COLUMN environments.public_endpoint IS
    'Provider-realized public HTTPS endpoint; port, private target, proxy configuration, TLS, health, and policy remain provider-owned.';
COMMENT ON COLUMN environments.desired_power IS
    'Lightspeed-owned power intent (running|paused|suspended|stopped); converged by the lifecycle reconciler, observed state is status.';
COMMENT ON COLUMN environments.idle_policy_json IS
    'Optional staged idle policy {pauseAfterMs,suspendAfterMs,stopAfterMs,closeAfterMs}; applied by the power reaper from the daemon idle report.';
COMMENT ON COLUMN environment_incarnations.power_states_json IS
    'Provider-reported power states this target supports, observed with the target id.';
COMMENT ON TABLE environment_credentials IS
    'Universe-owned credential bindings for an environment.';

COMMENT ON TABLE environment_registration_keys IS
    'Reusable universe-scoped admission policies for outbound envd registration; each key is also the group of the environments it admitted.';
COMMENT ON COLUMN environments.daemon_public_key IS
    'Lowercase hex Ed25519 public key of the registered daemon; the identity, unique across the deployment including closed rows.';
COMMENT ON COLUMN environments.last_seen_at_ms IS
    'Gateway heartbeat stamp on the registered control connection; stale under ready means the gateway stopped without recording the disconnect.';
