-- P80 G2 / P86 G1: runtime environment registry.
--
-- Providers register runtime capacity and expose host-protocol controllers.
-- Session environment bindings materialize provider targets into session-visible
-- env:<id> targets. The deterministic engine records only the selected target.

CREATE TABLE IF NOT EXISTS environment_providers (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    provider_id text NOT NULL,
    provider_kind text NOT NULL,
    display_name text,
    status text NOT NULL,
    controller_connection_json jsonb NOT NULL,
    capabilities_json jsonb NOT NULL,
    implementation_json jsonb NOT NULL,
    last_seen_ms bigint NOT NULL,
    lease_expires_ms bigint NOT NULL,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, provider_id),

    CONSTRAINT environment_providers_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_providers_kind_known
        CHECK (provider_kind IN ('sandbox', 'bridge', 'custom')),
    CONSTRAINT environment_providers_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT environment_providers_status_known
        CHECK (status IN ('registering', 'online', 'stale', 'offline', 'disabled')),
    CONSTRAINT environment_providers_controller_connection_object
        CHECK (jsonb_typeof(controller_connection_json) = 'object'),
    CONSTRAINT environment_providers_capabilities_object
        CHECK (jsonb_typeof(capabilities_json) = 'object'),
    CONSTRAINT environment_providers_implementation_object
        CHECK (jsonb_typeof(implementation_json) = 'object'),
    CONSTRAINT environment_providers_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT environment_providers_last_seen_nonnegative
        CHECK (last_seen_ms >= 0),
    CONSTRAINT environment_providers_lease_nonnegative
        CHECK (lease_expires_ms >= 0),
    CONSTRAINT environment_providers_lease_after_seen
        CHECK (lease_expires_ms >= last_seen_ms),
    CONSTRAINT environment_providers_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT environment_providers_updated_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT environment_providers_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS environment_providers_status_idx
    ON environment_providers (universe_id, status, provider_id);

CREATE INDEX IF NOT EXISTS environment_providers_kind_idx
    ON environment_providers (universe_id, provider_kind, provider_id);

CREATE TABLE IF NOT EXISTS environment_targets (
    universe_id uuid NOT NULL,
    provider_id text NOT NULL,
    target_id text NOT NULL,
    display_name text,
    status text NOT NULL,
    scope_json jsonb NOT NULL,
    capabilities_json jsonb NOT NULL,
    default_cwd text,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    observed_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, provider_id, target_id),

    FOREIGN KEY (universe_id, provider_id)
        REFERENCES environment_providers (universe_id, provider_id)
        ON DELETE CASCADE,

    CONSTRAINT environment_targets_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_targets_target_id_format
        CHECK (target_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_targets_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT environment_targets_status_known
        CHECK (
            status IN (
                'creating',
                'starting',
                'ready',
                'stopped',
                'closing',
                'closed',
                'failed',
                'unknown'
            )
        ),
    CONSTRAINT environment_targets_scope_object
        CHECK (jsonb_typeof(scope_json) = 'object'),
    CONSTRAINT environment_targets_capabilities_object
        CHECK (jsonb_typeof(capabilities_json) = 'object'),
    CONSTRAINT environment_targets_default_cwd_not_empty
        CHECK (default_cwd IS NULL OR default_cwd <> ''),
    CONSTRAINT environment_targets_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT environment_targets_observed_nonnegative
        CHECK (observed_at_ms >= 0)
);

CREATE INDEX IF NOT EXISTS environment_targets_status_idx
    ON environment_targets (universe_id, status, provider_id, target_id);

CREATE TABLE IF NOT EXISTS session_environment_bindings (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    env_id text NOT NULL,
    provider_id text NOT NULL,
    target_id text NOT NULL,
    exec_target_json jsonb NOT NULL,
    kind text NOT NULL,
    status text NOT NULL,
    capabilities_json jsonb NOT NULL,
    connection_json jsonb NOT NULL,
    cwd text,
    fs_routes_json jsonb NOT NULL DEFAULT '[]',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, session_id, env_id),

    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id)
        ON DELETE CASCADE,
    FOREIGN KEY (universe_id, provider_id)
        REFERENCES environment_providers (universe_id, provider_id)
        ON DELETE CASCADE,
    FOREIGN KEY (universe_id, provider_id, target_id)
        REFERENCES environment_targets (universe_id, provider_id, target_id)
        ON DELETE CASCADE,

    CONSTRAINT session_environment_bindings_env_id_format
        CHECK (env_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT session_environment_bindings_exec_target_object
        CHECK (jsonb_typeof(exec_target_json) = 'object'),
    CONSTRAINT session_environment_bindings_kind_known
        CHECK (kind IN ('sandbox', 'attached_host')),
    CONSTRAINT session_environment_bindings_status_known
        CHECK (status IN ('attaching', 'ready', 'degraded', 'detached')),
    CONSTRAINT session_environment_bindings_capabilities_object
        CHECK (jsonb_typeof(capabilities_json) = 'object'),
    CONSTRAINT session_environment_bindings_connection_object
        CHECK (jsonb_typeof(connection_json) = 'object'),
    CONSTRAINT session_environment_bindings_cwd_not_empty
        CHECK (cwd IS NULL OR cwd <> ''),
    CONSTRAINT session_environment_bindings_fs_routes_array
        CHECK (jsonb_typeof(fs_routes_json) = 'array'),
    CONSTRAINT session_environment_bindings_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT session_environment_bindings_updated_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT session_environment_bindings_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS session_environment_bindings_provider_idx
    ON session_environment_bindings (universe_id, provider_id, target_id);

CREATE INDEX IF NOT EXISTS session_environment_bindings_session_status_idx
    ON session_environment_bindings (universe_id, session_id, status, env_id);

-- This table is a Lightspeed-side handle ledger only. The environment
-- provider remains the source of truth for job status, output, dependencies,
-- exit codes, and artifacts.
CREATE TABLE IF NOT EXISTS environment_jobs (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    env_id text NOT NULL,
    provider_id text NOT NULL,
    target_id text NOT NULL,
    namespace text NOT NULL,
    job_id text NOT NULL,
    name text,
    queue_key text,
    created_by_run_id bigint,
    created_by_turn_id bigint,
    created_by_tool_call_id text,
    created_at_ms bigint NOT NULL,
    start_request_hash text NOT NULL,

    PRIMARY KEY (universe_id, session_id, env_id, job_id),

    FOREIGN KEY (universe_id, session_id, env_id)
        REFERENCES session_environment_bindings (universe_id, session_id, env_id)
        ON DELETE CASCADE,
    FOREIGN KEY (universe_id, provider_id, target_id)
        REFERENCES environment_targets (universe_id, provider_id, target_id)
        ON DELETE CASCADE,

    CONSTRAINT environment_jobs_env_id_format
        CHECK (env_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_jobs_provider_id_format
        CHECK (provider_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_jobs_target_id_format
        CHECK (target_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_jobs_namespace_format
        CHECK (namespace ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_jobs_namespace_session
        CHECK (namespace = session_id),
    CONSTRAINT environment_jobs_job_id_format
        CHECK (job_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_jobs_name_not_empty
        CHECK (name IS NULL OR name <> ''),
    CONSTRAINT environment_jobs_queue_key_format
        CHECK (queue_key IS NULL OR queue_key ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT environment_jobs_created_by_run_nonnegative
        CHECK (created_by_run_id IS NULL OR created_by_run_id >= 0),
    CONSTRAINT environment_jobs_created_by_turn_nonnegative
        CHECK (created_by_turn_id IS NULL OR created_by_turn_id >= 0),
    CONSTRAINT environment_jobs_tool_call_id_not_empty
        CHECK (created_by_tool_call_id IS NULL OR created_by_tool_call_id <> ''),
    CONSTRAINT environment_jobs_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT environment_jobs_hash_not_empty
        CHECK (start_request_hash <> '')
);

CREATE INDEX IF NOT EXISTS environment_jobs_session_env_idx
    ON environment_jobs (universe_id, session_id, env_id, job_id);

CREATE INDEX IF NOT EXISTS environment_jobs_session_latest_idx
    ON environment_jobs (universe_id, session_id, created_at_ms DESC, env_id, job_id);

CREATE INDEX IF NOT EXISTS environment_jobs_provider_idx
    ON environment_jobs (universe_id, provider_id, target_id, namespace, job_id);

COMMENT ON TABLE environment_providers IS
    'Universe-scoped runtime environment providers that advertise host-protocol controllers.';
COMMENT ON TABLE environment_targets IS
    'Mirrored host-protocol targets observed from registered environment providers.';
COMMENT ON TABLE session_environment_bindings IS
    'Session-visible env:<id> bindings to provider targets and host data-plane connections.';
COMMENT ON TABLE environment_jobs IS
    'Session-owned environment job handle ledger for routing and idempotency only.';
COMMENT ON COLUMN environment_jobs.namespace IS
    'Provider-facing job namespace sent to the environment executor. In v1 this is derived from and constrained to session_id, but it is stored separately because providers route by namespace rather than Lightspeed session id.';
