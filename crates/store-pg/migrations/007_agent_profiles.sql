-- P85: agent profile catalog.
--
-- Profiles are universe-scoped declarative provisioning documents. They never
-- store credentials or runtime state; session-visible setup is materialized by
-- the hosted runtime into existing session/resource operations.

CREATE TABLE IF NOT EXISTS agent_profiles (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    profile_id text NOT NULL,
    display_name text,
    description text,
    revision bigint NOT NULL,
    document_json jsonb NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, profile_id),

    CONSTRAINT agent_profiles_profile_id_format
        CHECK (profile_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT agent_profiles_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT agent_profiles_description_not_empty
        CHECK (description IS NULL OR description <> ''),
    CONSTRAINT agent_profiles_revision_positive
        CHECK (revision > 0),
    CONSTRAINT agent_profiles_document_object
        CHECK (jsonb_typeof(document_json) = 'object'),
    CONSTRAINT agent_profiles_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT agent_profiles_updated_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT agent_profiles_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS agent_profiles_updated_idx
    ON agent_profiles (universe_id, updated_at_ms DESC, profile_id);

COMMENT ON TABLE agent_profiles IS
    'Universe-scoped agent profile catalog; profiles are declarative setup documents applied by the hosted runtime.';
COMMENT ON COLUMN agent_profiles.document_json IS
    'Serialized ProfileDocument. Contains references only, never secrets.';
