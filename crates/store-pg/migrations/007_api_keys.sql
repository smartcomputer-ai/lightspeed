-- Inbound API keys. A key names what it reaches (one universe, or the
-- deployment when universe_id is null), the method groups it may call, and
-- whether it may assert the actor a request acts for. That is its whole
-- authority: people, roles and groups live with whoever asserts actors.
--
-- Only the SHA-256 hash of the server-generated secret is stored; the
-- plaintext is shown once at mint time. key_prefix is the caller-facing
-- handle for listing and revocation. Keys are immutable apart from their
-- display name and revocation: changing what a key may do is revoking it and
-- minting another. Group names are validated by the runtime at mint.
CREATE TABLE IF NOT EXISTS api_keys (
    key_hash text PRIMARY KEY CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    key_prefix text NOT NULL UNIQUE CHECK (key_prefix <> ''),
    universe_id uuid REFERENCES universes (universe_id) ON DELETE CASCADE,
    groups text[] NOT NULL CHECK (cardinality(groups) > 0),
    assert_actor boolean NOT NULL DEFAULT false,
    display_name text,
    -- Who minted it: attribution only, no foreign key.
    created_by jsonb NOT NULL,
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0),
    revoked_at_ms bigint CHECK (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
    last_used_at_ms bigint
);
CREATE INDEX IF NOT EXISTS api_keys_universe_idx ON api_keys (universe_id);
