-- Initial PostgreSQL schema for Forge sessions and content-addressed storage.
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
    created_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT universes_slug_format
        CHECK (slug IS NULL OR slug ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$')
);

CREATE TABLE IF NOT EXISTS sessions (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    session_id text NOT NULL,
    agent_handle text NOT NULL,
    head_seq bigint,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, session_id),

    CONSTRAINT sessions_session_id_format
        CHECK (session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_agent_handle_format
        CHECK (agent_handle ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_head_seq_positive
        CHECK (head_seq IS NULL OR head_seq > 0),
    CONSTRAINT sessions_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT sessions_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT sessions_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS sessions_agent_handle_session_id_idx
    ON sessions (universe_id, agent_handle, session_id);

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
    inserted_at timestamptz NOT NULL DEFAULT now(),

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
    inserted_at timestamptz NOT NULL DEFAULT now(),

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

CREATE TABLE IF NOT EXISTS session_blob_roots (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    digest text NOT NULL,
    root_kind text NOT NULL DEFAULT 'session',
    first_seq bigint,
    last_seq bigint,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, session_id, digest, root_kind),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (universe_id, digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,

    CONSTRAINT session_blob_roots_root_kind_present
        CHECK (root_kind <> ''),
    CONSTRAINT session_blob_roots_first_seq_positive
        CHECK (first_seq IS NULL OR first_seq > 0),
    CONSTRAINT session_blob_roots_last_seq_positive
        CHECK (last_seq IS NULL OR last_seq > 0),
    CONSTRAINT session_blob_roots_seq_order
        CHECK (
            first_seq IS NULL
            OR last_seq IS NULL
            OR last_seq >= first_seq
        )
);

CREATE INDEX IF NOT EXISTS session_blob_roots_digest_idx
    ON session_blob_roots (universe_id, digest);

CREATE TABLE IF NOT EXISTS cas_blob_edges (
    universe_id uuid NOT NULL,
    parent_digest text NOT NULL,
    child_digest text NOT NULL,
    edge_kind text NOT NULL DEFAULT 'contains',
    inserted_at timestamptz NOT NULL DEFAULT now(),

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

CREATE TABLE IF NOT EXISTS vfs_snapshots (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    digest text NOT NULL,
    snapshot_ref text GENERATED ALWAYS AS ('sha256:' || digest) STORED,
    source_json jsonb NOT NULL,
    display_name text,
    created_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, digest),
    FOREIGN KEY (universe_id, digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,

    CONSTRAINT vfs_snapshots_digest_format
        CHECK (digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT vfs_snapshots_source_is_object
        CHECK (jsonb_typeof(source_json) = 'object'),
    CONSTRAINT vfs_snapshots_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS vfs_snapshots_snapshot_ref_idx
    ON vfs_snapshots (universe_id, snapshot_ref);

CREATE TABLE IF NOT EXISTS vfs_workspaces (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    workspace_id text NOT NULL,
    base_snapshot_digest text,
    head_snapshot_digest text NOT NULL,
    revision bigint NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, workspace_id),
    FOREIGN KEY (universe_id, base_snapshot_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,
    FOREIGN KEY (universe_id, head_snapshot_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,

    CONSTRAINT vfs_workspaces_workspace_id_format
        CHECK (workspace_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT vfs_workspaces_base_digest_format
        CHECK (base_snapshot_digest IS NULL OR base_snapshot_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT vfs_workspaces_head_digest_format
        CHECK (head_snapshot_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT vfs_workspaces_revision_nonnegative
        CHECK (revision >= 0),
    CONSTRAINT vfs_workspaces_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT vfs_workspaces_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT vfs_workspaces_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS vfs_workspaces_head_digest_idx
    ON vfs_workspaces (universe_id, head_snapshot_digest);

CREATE TABLE IF NOT EXISTS vfs_mounts (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    session_id text NOT NULL,
    mount_path text NOT NULL,
    source_kind text NOT NULL,
    snapshot_digest text,
    workspace_id text,
    access text NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, session_id, mount_path),
    FOREIGN KEY (universe_id, snapshot_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,
    FOREIGN KEY (universe_id, workspace_id)
        REFERENCES vfs_workspaces (universe_id, workspace_id) ON DELETE CASCADE,

    CONSTRAINT vfs_mounts_session_id_format
        CHECK (session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT vfs_mounts_mount_path_absolute
        CHECK (mount_path LIKE '/%'),
    CONSTRAINT vfs_mounts_source_kind_known
        CHECK (source_kind IN ('snapshot', 'workspace')),
    CONSTRAINT vfs_mounts_access_known
        CHECK (access IN ('read_only', 'read_write')),
    CONSTRAINT vfs_mounts_source_shape
        CHECK (
            (
                source_kind = 'snapshot'
                AND snapshot_digest IS NOT NULL
                AND workspace_id IS NULL
            )
            OR
            (
                source_kind = 'workspace'
                AND snapshot_digest IS NULL
                AND workspace_id IS NOT NULL
            )
        )
);

CREATE INDEX IF NOT EXISTS vfs_mounts_session_idx
    ON vfs_mounts (universe_id, session_id);

COMMENT ON TABLE universes IS
    'Tenant/project/workspace boundary; sessions and CAS are shared within one universe.';
COMMENT ON TABLE sessions IS
    'One row per Forge session; head_seq is updated transactionally with event appends.';
COMMENT ON TABLE session_events IS
    'Append-only dynamic session entries stored as canonical JSONB with generated query columns.';
COMMENT ON TABLE cas_blobs IS
    'Universe-scoped CAS catalog keyed by sha256 digest; small payloads inline, large payloads external.';
COMMENT ON TABLE session_blob_roots IS
    'Session-scoped CAS roots used by future reachability and garbage collection.';
COMMENT ON TABLE cas_blob_edges IS
    'Optional best-effort parent-child CAS edges recorded outside put_bytes.';
COMMENT ON TABLE vfs_snapshots IS
    'Descriptive metadata for immutable CAS-backed VFS snapshot manifests.';
COMMENT ON TABLE vfs_workspaces IS
    'Mutable workspace heads pointing at immutable VFS snapshot refs.';
COMMENT ON TABLE vfs_mounts IS
    'Session-visible VFS mount records for snapshot and workspace roots.';
