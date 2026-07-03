-- Deployment-scoped API keys for inbound gateway authentication (P90 Phase 2).
--
-- Design notes:
-- - This is the second deployment-level table after `universes`, and the
--   first inbound-auth table: resolution happens before a universe is known
--   (the key IS the universe selector), so rows are not universe-scoped;
--   universe_id is a value column resolved by the lookup.
-- - Only the SHA-256 hash of the server-generated secret is stored. No KDF
--   (secrets are high-entropy random strings, not human passwords) and no
--   AEAD/master-key involvement (the secret never needs to be recovered,
--   only recognized). The plaintext is shown once at mint time.
-- - key_prefix is the caller-facing handle for listing and revocation; it is
--   the first characters of the secret and reveals no meaningful entropy.
-- - principal_kind/principal_id mirror auth_grants: the principal stamped
--   onto grants/flows created through this key, recorded for audit.

CREATE TABLE IF NOT EXISTS api_keys (
    key_hash text PRIMARY KEY,
    key_prefix text NOT NULL,
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    principal_kind text NOT NULL DEFAULT 'universe_default',
    principal_id text,
    display_name text,
    created_at_ms bigint NOT NULL,
    revoked_at_ms bigint,
    last_used_at_ms bigint,

    CONSTRAINT api_keys_key_hash_format
        CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT api_keys_key_prefix_not_empty
        CHECK (key_prefix <> ''),
    CONSTRAINT api_keys_principal_kind_valid
        CHECK (principal_kind IN ('user', 'service_account', 'universe_default')),
    CONSTRAINT api_keys_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT api_keys_revoked_after_created
        CHECK (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms)
);

CREATE UNIQUE INDEX IF NOT EXISTS api_keys_key_prefix_idx
    ON api_keys (key_prefix);

CREATE INDEX IF NOT EXISTS api_keys_universe_idx
    ON api_keys (universe_id);
