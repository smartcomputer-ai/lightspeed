-- P118 environment compute: deployment providers, universe bindings, and
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

CREATE TABLE IF NOT EXISTS environments (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    environment_id text NOT NULL,
    request_id text NOT NULL,
    source_kind text NOT NULL,
    provider_id text,
    binding_id text,
    display_name text,
    status text NOT NULL,
    current_incarnation_id text NOT NULL,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, environment_id),
    UNIQUE (universe_id, request_id),
    FOREIGN KEY (universe_id, binding_id)
        REFERENCES environment_provider_bindings (universe_id, binding_id) ON DELETE RESTRICT,
    CONSTRAINT environments_ids_format CHECK (
        environment_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        AND request_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
        AND current_incarnation_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'
    ),
    CONSTRAINT environments_source_known CHECK (source_kind IN ('provisioned', 'enrolled')),
    CONSTRAINT environments_source_fields CHECK (
        (source_kind = 'provisioned' AND provider_id IS NOT NULL AND binding_id IS NOT NULL)
        OR (source_kind = 'enrolled' AND provider_id IS NULL AND binding_id IS NULL)
    ),
    CONSTRAINT environments_status_known CHECK (status IN (
        'provisioning', 'booting', 'waiting_for_daemon', 'ready', 'offline',
        'closing', 'closed', 'failed', 'unknown'
    )),
    CONSTRAINT environments_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT environments_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

CREATE INDEX IF NOT EXISTS environments_binding_status_idx
    ON environments (universe_id, binding_id, status, environment_id);

CREATE TABLE IF NOT EXISTS environment_incarnations (
    universe_id uuid NOT NULL,
    environment_id text NOT NULL,
    incarnation_id text NOT NULL,
    provision_request_id text,
    provider_target_id text,
    template_id text,
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
    ),
    CONSTRAINT environment_incarnations_source_fields CHECK (
        (
            provision_request_id IS NOT NULL
            AND template_id IS NOT NULL
        )
        OR (
            provision_request_id IS NULL
            AND provider_target_id IS NULL
            AND template_id IS NULL
        )
    ),
    CONSTRAINT environment_incarnations_times_valid CHECK (
        created_at_ms >= 0 AND updated_at_ms >= created_at_ms
    )
);

CREATE TABLE IF NOT EXISTS environment_daemon_enrollments (
    universe_id uuid NOT NULL,
    environment_id text NOT NULL,
    incarnation_id text NOT NULL,
    ticket_hash bytea NOT NULL,
    ticket_expires_at_ms bigint NOT NULL,
    ticket_redeemed_at_ms bigint,
    revoked_at_ms bigint,
    daemon_id text,
    daemon_public_key bytea,
    enrolled_at_ms bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    PRIMARY KEY (universe_id, environment_id, incarnation_id),
    FOREIGN KEY (universe_id, environment_id, incarnation_id)
        REFERENCES environment_incarnations (universe_id, environment_id, incarnation_id)
        ON DELETE CASCADE,
    CONSTRAINT environment_daemon_enrollments_ticket_hash_size
        CHECK (octet_length(ticket_hash) = 32),
    CONSTRAINT environment_daemon_enrollments_identity_complete CHECK (
        (daemon_id IS NULL AND daemon_public_key IS NULL AND enrolled_at_ms IS NULL)
        OR (
            daemon_id IS NOT NULL
            AND daemon_public_key IS NOT NULL
            AND octet_length(daemon_public_key) = 32
            AND enrolled_at_ms IS NOT NULL
        )
    ),
    CONSTRAINT environment_daemon_enrollments_times_valid CHECK (
        created_at_ms >= 0
        AND updated_at_ms >= created_at_ms
        AND ticket_expires_at_ms >= created_at_ms
    )
);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'environments_current_incarnation_fk'
    ) THEN
        ALTER TABLE environments ADD CONSTRAINT environments_current_incarnation_fk
            FOREIGN KEY (universe_id, environment_id, current_incarnation_id)
            REFERENCES environment_incarnations (universe_id, environment_id, incarnation_id)
            DEFERRABLE INITIALLY DEFERRED;
    END IF;
END $$;

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

DROP TABLE IF EXISTS environment_jobs;
DROP TABLE IF EXISTS environment_job_groups;

COMMENT ON TABLE environment_providers IS
    'Operator-registered provider identity and controller connection; protocol and presence are observed transiently.';
COMMENT ON COLUMN environment_providers.metadata_json IS
    'Non-authoritative operator metadata; never provider capability, health, or allocation policy.';
COMMENT ON TABLE environment_provider_bindings IS
    'Revisioned universe routing and admission binding to one provider; allocation and ingress policy remain provider-owned.';
COMMENT ON TABLE environment_daemon_enrollments IS
    'One-time bootstrap ticket and daemon public-key identity for a directly enrolled environment incarnation. Provider-mediated environments authenticate through their provider binding and do not have rows here; live route presence remains gateway-memory state.';
COMMENT ON COLUMN environment_provider_bindings.metadata_json IS
    'Non-authoritative binding labels; never provider template, quota, capacity, or ingress policy.';
COMMENT ON TABLE environments IS
    'Universe-owned logical environment lifecycle intent; not a physical resource reservation ledger.';
COMMENT ON TABLE environment_incarnations IS
    'Lightspeed-authorized environment generations with stable provider retry and target linkage; not provider inventory or live gateway state.';
COMMENT ON COLUMN environment_incarnations.provider_target_id IS
    'Opaque provider-scoped target handle returned by createTarget; interpreted with the environment provider identity.';
COMMENT ON TABLE environment_credentials IS
    'Universe-owned credential bindings for an environment.';
