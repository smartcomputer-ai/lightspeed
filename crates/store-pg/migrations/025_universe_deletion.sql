ALTER TABLE universes ADD COLUMN deleting_at_ms bigint;
ALTER TABLE universes ADD COLUMN deletion_json jsonb;
ALTER TABLE universes ADD CONSTRAINT universes_deletion_state
    CHECK ((deleting_at_ms IS NULL) = (deletion_json IS NULL));
ALTER TABLE universes DROP CONSTRAINT universes_handle_key;
CREATE UNIQUE INDEX universes_handle_key ON universes (handle) WHERE deleting_at_ms IS NULL;

CREATE TABLE universes_deleted (
    universe_id uuid PRIMARY KEY,
    handle text NOT NULL,
    display_name text,
    created_at_ms bigint NOT NULL,
    deleting_at_ms bigint NOT NULL,
    deleted_at_ms bigint NOT NULL,
    requested_by jsonb NOT NULL,
    summary jsonb NOT NULL
);
CREATE INDEX universes_deleted_newest ON universes_deleted (deleted_at_ms DESC, universe_id DESC);
ALTER TABLE environments DROP CONSTRAINT environments_close_reason;
ALTER TABLE environments ADD CONSTRAINT environments_close_reason CHECK (
    (close_reason IS NULL) = (state NOT IN ('closing', 'closed'))
    AND (close_reason IS NULL OR close_reason IN (
        'requested', 'key_revoked', 'ephemeral_grace', 'departed',
        'offline_timeout', 'provider_ended', 'universe_deleted'
    ))
);
