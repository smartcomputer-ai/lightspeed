-- Core PostgreSQL schema for Lightspeed sessions and content-addressed storage.
--
-- Design notes:
-- - Postgres is the source of truth for session logs and CAS metadata.
-- - A universe is the tenant/project/workspace boundary.
-- - Sessions and agents share CAS within a universe.
-- - CAS metadata and object keys are isolated between universes.
-- - Small CAS payloads are stored inline in bytea.
-- - Large CAS payloads are stored externally; object_key points at the bytes.
-- - Packed CAS objects are intentionally omitted from v1. put_many can batch
--   hashes, external uploads, and INSERTs without changing this schema.
-- - Generated columns require PostgreSQL 12 or newer.

CREATE TABLE IF NOT EXISTS universes (
    universe_id uuid PRIMARY KEY,
    slug text UNIQUE,

    CONSTRAINT universes_slug_format
        CHECK (slug IS NULL OR slug ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$')
);

CREATE TABLE IF NOT EXISTS sessions (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    session_id text NOT NULL,
    head_seq bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    -- Clone/fork lineage. A session may be created by copying another session's
    -- config ("clone": same config, fresh log) or by branching its event log
    -- ("fork": the parent's events are inherited by reference, not copied, and
    -- this session's own log continues from the branch point).
    -- source_session_id records the content origin; NULL for a fresh root
    -- session. source_seq distinguishes the two cases:
    --   NULL  -> config-only clone; child log starts at seq 1.
    --   set   -> history fork; 0 means an empty inherited prefix, otherwise
    --            the child's effective log is the parent's events
    --            1..source_seq (read by reference, recursively if the parent is
    --            itself a fork) followed by this session's own rows, which start
    --            at source_seq + 1. The parent's events ARE NOT copied; the seq
    --            line stays contiguous across the chain so reads stitch without
    --            remapping. Upstream segments are clamped to source_seq, so a
    --            fork is a branch, not a shared tail of a still-growing parent.
    -- This only records where content came from; who initiated the
    -- clone/fork is unrelated and, if needed, is expressed as a session_link.
    source_session_id text,
    source_seq bigint,

    PRIMARY KEY (universe_id, session_id),

    FOREIGN KEY (universe_id, source_session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE SET NULL,

    CONSTRAINT sessions_session_id_format
        CHECK (session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_head_seq_positive
        CHECK (head_seq IS NULL OR head_seq > 0),
    CONSTRAINT sessions_source_seq_nonnegative
        CHECK (source_seq IS NULL OR source_seq >= 0),
    CONSTRAINT sessions_source_seq_requires_source
        CHECK (source_seq IS NULL OR source_session_id IS NOT NULL),
    CONSTRAINT sessions_source_not_self
        CHECK (source_session_id IS NULL OR source_session_id <> session_id),
    CONSTRAINT sessions_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT sessions_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT sessions_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

ALTER TABLE sessions
    ADD COLUMN IF NOT EXISTS source_session_id text;

ALTER TABLE sessions
    ADD COLUMN IF NOT EXISTS source_seq bigint;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'sessions'::regclass
          AND conname = 'sessions_source_session_id_fkey'
    ) THEN
        ALTER TABLE sessions
            ADD CONSTRAINT sessions_source_session_id_fkey
            FOREIGN KEY (universe_id, source_session_id)
            REFERENCES sessions (universe_id, session_id) ON DELETE SET NULL;
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'sessions'::regclass
          AND conname = 'sessions_source_seq_requires_source'
    ) THEN
        ALTER TABLE sessions
            ADD CONSTRAINT sessions_source_seq_requires_source
            CHECK (source_seq IS NULL OR source_session_id IS NOT NULL);
    END IF;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'sessions'::regclass
          AND conname = 'sessions_source_not_self'
    ) THEN
        ALTER TABLE sessions
            ADD CONSTRAINT sessions_source_not_self
            CHECK (source_session_id IS NULL OR source_session_id <> session_id);
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS sessions_source_session_id_idx
    ON sessions (universe_id, source_session_id)
    WHERE source_session_id IS NOT NULL;

ALTER TABLE sessions
    DROP CONSTRAINT IF EXISTS sessions_source_seq_positive;

ALTER TABLE sessions
    DROP CONSTRAINT IF EXISTS sessions_source_seq_nonnegative;

ALTER TABLE sessions
    ADD CONSTRAINT sessions_source_seq_nonnegative
        CHECK (source_seq IS NULL OR source_seq >= 0);

-- Directed, typed relationships between sessions. A link means
-- "from_session_id can <relationship> to_session_id" — for example which
-- sessions an agent may see, access, or configure. Links are plain data in
-- v1: nothing enforces them yet; Fleet tooling reads them later. They are set
-- independently of clone/fork lineage and can be created manually between any
-- two existing sessions, including sessions that never spawned each other.
-- relationship is an open string so the vocabulary can grow without migration.
CREATE TABLE IF NOT EXISTS session_links (
    universe_id uuid NOT NULL,
    from_session_id text NOT NULL,
    to_session_id text NOT NULL,
    relationship text NOT NULL,
    created_at_ms bigint NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,

    PRIMARY KEY (universe_id, from_session_id, to_session_id, relationship),
    FOREIGN KEY (universe_id, from_session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (universe_id, to_session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,

    CONSTRAINT session_links_relationship_present
        CHECK (relationship <> ''),
    CONSTRAINT session_links_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT session_links_metadata_is_object
        CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE INDEX IF NOT EXISTS session_links_to_session_id_idx
    ON session_links (universe_id, to_session_id);

CREATE TABLE IF NOT EXISTS session_events (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    entry_json jsonb NOT NULL,
    seq bigint GENERATED ALWAYS AS
        ((entry_json #>> '{position,seq}')::bigint) STORED,
    observed_at_ms bigint GENERATED ALWAYS AS
        ((entry_json ->> 'observed_at_ms')::bigint) STORED,
    event_kind text GENERATED ALWAYS AS
        (entry_json #>> '{event,kind}') STORED,
    event_version integer GENERATED ALWAYS AS
        ((entry_json #>> '{event,version}')::integer) STORED,

    PRIMARY KEY (universe_id, session_id, seq),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,

    CONSTRAINT session_events_seq_positive
        CHECK (seq > 0),
    CONSTRAINT session_events_observed_at_ms_nonnegative
        CHECK (observed_at_ms IS NOT NULL AND observed_at_ms >= 0),
    CONSTRAINT session_events_event_kind_present
        CHECK (event_kind IS NOT NULL AND event_kind <> ''),
    CONSTRAINT session_events_event_version_positive
        CHECK (event_version IS NOT NULL AND event_version > 0),
    CONSTRAINT session_events_entry_is_object
        CHECK (jsonb_typeof(entry_json) = 'object'),
    CONSTRAINT session_events_joins_is_object
        CHECK (
            entry_json #> '{joins}' IS NOT NULL
            AND jsonb_typeof(entry_json #> '{joins}') = 'object'
        ),
    CONSTRAINT session_events_event_payload_present
        CHECK (entry_json #> '{event,payload}' IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS session_events_event_kind_idx
    ON session_events (universe_id, event_kind);

CREATE TABLE IF NOT EXISTS cas_blobs (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    digest text NOT NULL,
    blob_ref text GENERATED ALWAYS AS ('sha256:' || digest) STORED,
    byte_len bigint NOT NULL,
    storage_kind text NOT NULL,
    inline_bytes bytea,
    object_key text,
    object_etag text,
    object_version text,

    PRIMARY KEY (universe_id, digest),

    CONSTRAINT cas_blobs_digest_format
        CHECK (digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT cas_blobs_byte_len_nonnegative
        CHECK (byte_len >= 0),
    CONSTRAINT cas_blobs_storage_kind_known
        CHECK (storage_kind IN ('inline', 'object')),
    CONSTRAINT cas_blobs_inline_or_object
        CHECK (
            (
                storage_kind = 'inline'
                AND inline_bytes IS NOT NULL
                AND object_key IS NULL
                AND object_etag IS NULL
                AND object_version IS NULL
                AND byte_len = octet_length(inline_bytes)
            )
            OR
            (
                storage_kind = 'object'
                AND inline_bytes IS NULL
                AND object_key IS NOT NULL
                AND object_key <> ''
            )
        )
);

CREATE UNIQUE INDEX IF NOT EXISTS cas_blobs_blob_ref_idx
    ON cas_blobs (universe_id, blob_ref);

CREATE UNIQUE INDEX IF NOT EXISTS cas_blobs_object_key_idx
    ON cas_blobs (object_key)
    WHERE object_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS cas_session_roots (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    digest text NOT NULL,
    root_kind text NOT NULL DEFAULT 'session',
    first_seq bigint,
    last_seq bigint,

    PRIMARY KEY (universe_id, session_id, digest, root_kind),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (universe_id, digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,

    CONSTRAINT cas_session_roots_root_kind_present
        CHECK (root_kind <> ''),
    CONSTRAINT cas_session_roots_first_seq_positive
        CHECK (first_seq IS NULL OR first_seq > 0),
    CONSTRAINT cas_session_roots_last_seq_positive
        CHECK (last_seq IS NULL OR last_seq > 0),
    CONSTRAINT cas_session_roots_seq_order
        CHECK (
            first_seq IS NULL
            OR last_seq IS NULL
            OR last_seq >= first_seq
        )
);

CREATE INDEX IF NOT EXISTS cas_session_roots_digest_idx
    ON cas_session_roots (universe_id, digest);

CREATE TABLE IF NOT EXISTS cas_blob_edges (
    universe_id uuid NOT NULL,
    parent_digest text NOT NULL,
    child_digest text NOT NULL,
    edge_kind text NOT NULL DEFAULT 'contains',

    PRIMARY KEY (universe_id, parent_digest, child_digest, edge_kind),
    FOREIGN KEY (universe_id, parent_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE CASCADE,
    FOREIGN KEY (universe_id, child_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,

    CONSTRAINT cas_blob_edges_edge_kind_present
        CHECK (edge_kind <> '')
);

CREATE INDEX IF NOT EXISTS cas_blob_edges_child_digest_idx
    ON cas_blob_edges (universe_id, child_digest);

COMMENT ON TABLE universes IS
    'Tenant/project/workspace boundary; sessions and CAS are shared within one universe.';
COMMENT ON TABLE sessions IS
    'One row per Lightspeed session; head_seq is updated transactionally with event appends. source_session_id/source_seq record clone (config-only) or fork (history-branch) lineage.';
COMMENT ON TABLE session_links IS
    'Directed, typed relationships between sessions (e.g. visibility/access/configure). Plain data in v1, not enforced; set independently of clone/fork lineage.';
COMMENT ON TABLE session_events IS
    'Append-only stored session entries as canonical JSONB with generated query columns.';
COMMENT ON TABLE cas_blobs IS
    'Universe-scoped CAS catalog keyed by sha256 digest; small payloads inline, large payloads external.';
COMMENT ON TABLE cas_session_roots IS
    'Session-scoped CAS roots used by future reachability and garbage collection.';
COMMENT ON TABLE cas_blob_edges IS
    'Optional best-effort parent-child CAS edges recorded outside put_bytes.';
