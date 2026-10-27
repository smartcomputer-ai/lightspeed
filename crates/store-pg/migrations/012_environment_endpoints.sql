-- Environment protocol v3 storage.
--
-- Design notes:
-- - Providers and external environments are reached at one validated
--   endpoint URL (ws://, wss:// or unix://) whose scheme selects the dialer;
--   the transport JSON documents are gone.
-- - The provider credential is the bearer token Lightspeed presents on the
--   controller and data routes. It is sealed with AES-256-GCM by the store
--   adapter, exactly like auth_secrets, and never stored in plaintext.
-- - controller_adopt_target, controller_ingress and data_route_base are what
--   the controller advertised at its last successful initialize: all three
--   or none.
-- - lifecycle_error records why an environment failed, or the latest error
--   its lifecycle is retrying past, instead of a caller metadata entry.
-- - Provider target ids follow the protocol's opaque rule.
-- - public_ingress_enabled is the ingress intent and public_endpoint what
--   the provider last reported realized; the reconciler converges the
--   endpoint toward the intent, so either may lead the other for a while.
--
-- Rows written in the earlier shape are not converted: purge providers and
-- external environments before migrating a database that has them.

ALTER TABLE environment_providers
    DROP CONSTRAINT environment_providers_json_objects;

ALTER TABLE environment_providers
    DROP COLUMN controller_connection_json,
    ADD COLUMN controller_endpoint text NOT NULL,
    ADD COLUMN credential_key_id text NOT NULL,
    ADD COLUMN credential_nonce bytea NOT NULL,
    ADD COLUMN credential_ciphertext bytea NOT NULL,
    ADD COLUMN controller_adopt_target boolean,
    ADD COLUMN controller_ingress boolean,
    ADD COLUMN data_route_base text,
    ADD CONSTRAINT environment_providers_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    ADD CONSTRAINT environment_providers_controller_endpoint_format CHECK (
        controller_endpoint ~ '^(wss?|unix)://[^[:space:][:cntrl:]]+$'
        AND octet_length(controller_endpoint) <= 2048
    ),
    ADD CONSTRAINT environment_providers_credential_sealed CHECK (
        credential_key_id <> '' AND octet_length(credential_nonce) = 12
    ),
    ADD CONSTRAINT environment_providers_controller_facts CHECK (
        (controller_adopt_target IS NULL) = (controller_ingress IS NULL)
        AND (controller_ingress IS NULL) = (data_route_base IS NULL)
    ),
    ADD CONSTRAINT environment_providers_data_route_base_format CHECK (
        data_route_base IS NULL
        OR (data_route_base ~ '^wss?://' AND octet_length(data_route_base) <= 2048)
    );

ALTER TABLE environments
    DROP CONSTRAINT environments_source_fields,
    DROP CONSTRAINT environments_public_ingress_fields;

ALTER TABLE environments
    DROP COLUMN daemon_connection_json,
    ADD COLUMN daemon_endpoint text,
    ADD COLUMN lifecycle_error text,
    ADD COLUMN lifecycle_error_first_at_ms bigint,
    ADD COLUMN lifecycle_error_last_at_ms bigint,
    ADD CONSTRAINT environments_source_fields CHECK (
        (
            source_kind = 'provisioned'
            AND provider_id IS NOT NULL AND binding_id IS NOT NULL
            AND daemon_endpoint IS NULL
            AND registration_key_id IS NULL AND daemon_id IS NULL
            AND daemon_public_key IS NULL AND identity_mode IS NULL
            AND last_seen_at_ms IS NULL
        )
        OR (
            source_kind = 'external'
            AND provider_id IS NULL AND binding_id IS NULL
            AND daemon_endpoint ~ '^(wss?|unix)://[^[:space:][:cntrl:]]+$'
            AND octet_length(daemon_endpoint) <= 2048
            AND registration_key_id IS NULL AND daemon_id IS NULL
            AND daemon_public_key IS NULL AND identity_mode IS NULL
            AND last_seen_at_ms IS NULL
        )
        OR (
            source_kind = 'registered'
            AND provider_id IS NULL AND binding_id IS NULL
            AND daemon_endpoint IS NULL
            AND registration_key_id IS NOT NULL
            AND daemon_id ~ '^daemon_[0-9a-f]{64}$'
            AND daemon_public_key ~ '^[0-9a-f]{64}$'
            AND identity_mode IN ('persistent', 'ephemeral')
            AND (last_seen_at_ms IS NULL OR last_seen_at_ms >= 0)
        )
    ),
    ADD CONSTRAINT environments_public_ingress_fields CHECK (
        (NOT public_ingress_enabled AND public_endpoint IS NULL)
        OR (
            source_kind = 'provisioned'
            AND (public_endpoint IS NULL OR public_endpoint <> '')
        )
    ),
    ADD CONSTRAINT environments_lifecycle_error_fields CHECK (
        (
            lifecycle_error IS NULL
            AND lifecycle_error_first_at_ms IS NULL
            AND lifecycle_error_last_at_ms IS NULL
        )
        OR (
            octet_length(lifecycle_error) BETWEEN 1 AND 1024
            AND lifecycle_error_first_at_ms >= 0
            AND lifecycle_error_last_at_ms >= lifecycle_error_first_at_ms
        )
    );

ALTER TABLE environment_incarnations
    ADD CONSTRAINT environment_incarnations_provider_target_id_opaque CHECK (
        provider_target_id IS NULL
        OR (
            octet_length(provider_target_id) BETWEEN 1 AND 255
            AND provider_target_id !~ '[[:cntrl:]]'
        )
    );

COMMENT ON TABLE environment_providers IS
    'Operator-registered provider identity, controller endpoint and sealed credential, plus the controller facts observed at its last initialize.';
COMMENT ON COLUMN environment_providers.controller_endpoint IS
    'Validated ws://, wss:// or unix:// URL of the provider controller; the scheme selects the dialer.';
COMMENT ON COLUMN environment_providers.credential_ciphertext IS
    'AES-256-GCM sealed bearer credential for the controller and data routes; AAD binds it to the provider id.';
COMMENT ON COLUMN environment_providers.data_route_base IS
    'Data route base (routes.data) the controller advertised at its last successful initialize; target data routes are built from it.';
COMMENT ON TABLE environments IS
    'Universe-owned logical environments. External environments store a Lightspeed-reachable envd endpoint URL; provisioned environments retain provider routing linkage.';
COMMENT ON COLUMN environments.public_ingress_enabled IS
    'Lightspeed-owned public ingress intent; the lifecycle reconciler converges the provider target toward it.';
COMMENT ON COLUMN environments.public_endpoint IS
    'Public endpoint the provider last reported realized; may lag public_ingress_enabled while the reconciler converges.';
COMMENT ON COLUMN environments.lifecycle_error IS
    'Why the environment failed, or the latest lifecycle error being retried; first/last instants bound the current run of errors.';
COMMENT ON COLUMN environment_incarnations.provider_target_id IS
    'Opaque provider-scoped target handle (1-255 bytes, no control characters); interpreted with the environment provider identity.';
