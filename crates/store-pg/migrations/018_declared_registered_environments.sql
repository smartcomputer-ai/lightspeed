-- A fixed machine can be declared as an environment before its daemon has
-- registered: a registered row with a caller-chosen id, the registration
-- name its daemon will present, and no daemon identity yet. The first
-- daemon its key admits under that name is bound to it, and a bound daemon
-- can be released again. So a registered row now has a declared name, a
-- daemon public key, or both, and a row with no daemon is never connected.
ALTER TABLE environments ADD COLUMN registration_name text;

ALTER TABLE environments DROP CONSTRAINT environments_source_fields;
ALTER TABLE environments ADD CONSTRAINT environments_source_fields CHECK (
    (
        source_kind = 'provisioned'
        AND request_id IS NOT NULL AND provider_id IS NOT NULL AND template_id IS NOT NULL
        AND daemon_endpoint IS NULL
        AND registration_key_id IS NULL AND registration_name IS NULL
        AND daemon_public_key IS NULL
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
        AND registration_key_id IS NULL AND registration_name IS NULL
        AND daemon_public_key IS NULL
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
        AND (daemon_public_key IS NOT NULL OR registration_name IS NOT NULL)
        AND (daemon_public_key IS NULL OR daemon_public_key ~ '^[0-9a-f]{64}$')
        AND (
            registration_name IS NULL
            OR (
                octet_length(registration_name) BETWEEN 1 AND 128
                AND registration_name !~ '[[:cntrl:]]'
            )
        )
        AND connected IS NOT NULL
        AND (daemon_public_key IS NOT NULL OR NOT connected)
        AND connection_changed_at_ms IS NOT NULL AND connection_changed_at_ms >= 0
        AND last_seen_at_ms IS NOT NULL AND last_seen_at_ms >= 0
    )
);

-- A name selects one environment of its key. A closed environment gives its
-- name back.
CREATE UNIQUE INDEX environments_registration_name_idx
    ON environments (universe_id, registration_key_id, registration_name)
    WHERE registration_name IS NOT NULL AND state <> 'closed';

-- A key may admit only the daemons whose name matches one of its declared
-- environments. Only a persistent key has declared environments.
ALTER TABLE environment_registration_keys
    ADD COLUMN declared_only boolean NOT NULL DEFAULT false;
ALTER TABLE environment_registration_keys
    ADD CONSTRAINT environment_registration_keys_declared_only_persistent
    CHECK (NOT declared_only OR identity_mode = 'persistent');
