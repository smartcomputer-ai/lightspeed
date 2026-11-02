-- Representative retained records and all six environment-owned tables in the
-- released schema. The deferred incarnation cycle is populated atomically.
BEGIN;
INSERT INTO universes (universe_id, slug) VALUES
    ('00000000-0000-0000-0000-000000000001', 'retained');
INSERT INTO sessions (universe_id, session_id, retention_root_session_id, created_at_ms, updated_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'retained-session', 'retained-session', 0, 0);
INSERT INTO auth_secrets (universe_id, secret_id, secret_kind, key_id, nonce, ciphertext, created_at_ms, updated_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'retained-secret', 'opaque', 'test', decode(repeat('00', 12), 'hex'), decode('01', 'hex'), 0, 0);
INSERT INTO environment_providers (provider_id, controller_connection_json, created_at_ms, updated_at_ms)
VALUES ('old-provider', '{}', 0, 0);
INSERT INTO environment_provider_bindings (universe_id, binding_id, provider_id, status, revision, created_at_ms, updated_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'old-binding', 'old-provider', 'enabled', 1, 0, 0);
INSERT INTO environment_registration_keys (universe_id, registration_key_id, display_name, key_prefix, secret_hash, identity_mode, created_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'old-key', 'Old key', 'old', repeat('a', 64), 'persistent', 0);
INSERT INTO environments (universe_id, environment_id, request_id, source_kind, daemon_connection_json, status, current_incarnation_id, created_at_ms, updated_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'old-environment', 'old-request', 'external', '{}', 'ready', 'old-incarnation', 0, 0);
INSERT INTO environment_incarnations (universe_id, environment_id, incarnation_id, created_at_ms, updated_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'old-environment', 'old-incarnation', 0, 0);
INSERT INTO environment_credentials (universe_id, environment_id, env_name, source_kind, secret_id, created_at_ms, updated_at_ms)
VALUES ('00000000-0000-0000-0000-000000000001', 'old-environment', 'TOKEN', 'direct_secret', 'retained-secret', 0, 0);
COMMIT;
