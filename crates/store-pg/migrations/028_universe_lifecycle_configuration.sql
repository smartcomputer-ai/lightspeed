-- Runtime resource configuration is independent of Platform accounts and grants.
ALTER TABLE universes
    DROP COLUMN platform_access,
    ADD COLUMN metadata_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN protected boolean NOT NULL DEFAULT false,
    ADD COLUMN expires_at_ms bigint,
    ADD CONSTRAINT universes_metadata_object CHECK (jsonb_typeof(metadata_json) = 'object'),
    ADD CONSTRAINT universes_protection_expiry CHECK (NOT protected OR expires_at_ms IS NULL),
    ADD CONSTRAINT universes_protected_deletion CHECK (NOT protected OR deleting_at_ms IS NULL),
    ADD CONSTRAINT universes_positive_expiry CHECK (expires_at_ms IS NULL OR expires_at_ms > 0);
CREATE INDEX universes_metadata_idx ON universes USING gin (metadata_json jsonb_path_ops);
CREATE INDEX universes_expiry_idx ON universes (expires_at_ms, universe_id)
    WHERE expires_at_ms IS NOT NULL AND deleting_at_ms IS NULL;
ALTER TABLE universes_deleted
    ADD COLUMN metadata_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD CONSTRAINT universes_deleted_metadata_object CHECK (jsonb_typeof(metadata_json) = 'object');
