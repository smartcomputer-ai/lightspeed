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
-- - Column-scoped lineage deletion requires PostgreSQL 15 or newer.

CREATE TABLE IF NOT EXISTS universes (
    universe_id uuid PRIMARY KEY,
    slug text UNIQUE,
    -- Server-side default so every insert path stamps creation time without
    -- threading a clock through `ensure_universe`.
    created_at_ms bigint NOT NULL
        DEFAULT ((extract(epoch FROM now()) * 1000)::bigint),

    CONSTRAINT universes_slug_format
        CHECK (slug IS NULL OR slug ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$')
);

CREATE TABLE IF NOT EXISTS sessions (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    session_id text NOT NULL,
    -- Human-readable name; store metadata only, never event-log state.
    display_name text,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    -- Materialized CoreAgent lifecycle for cheap list/filter operations. The
    -- event log remains authoritative and this is updated transactionally.
    lifecycle_status text NOT NULL DEFAULT 'new',
    closed_at_seq bigint,
    closed_at_ms bigint,
    -- History forks and delegated children share their owner's retention root;
    -- config-only clones are independent roots. Only roots carry a policy.
    retention_root_session_id text NOT NULL,
    delete_after_close_ms bigint,
    delete_at_ms bigint GENERATED ALWAYS AS
        (closed_at_ms + delete_after_close_ms) STORED,
    -- Cheap catalog projection of external lifecycle ownership. Workflow-tool
    -- declarations without a lifecycle controller do not make a session
    -- managed; controller/tool details remain in the log.
    managed boolean NOT NULL DEFAULT false,
    -- Who started it: the attribution of the request or bot worker, stamped
    -- once by the gateway right after the start. Null on delegated children,
    -- which read their root's, and on a root until the stamp.
    created_by jsonb,
    -- Who sees a root's tree: `restricted` (unshared) until it is shared with
    -- the universe, one way. Set on request-started roots only: a bot's
    -- session follows its bot, which is shared, a delegated child its root,
    -- and a root without one reads as unshared.
    visibility text CHECK (visibility IN ('universe', 'restricted')),
    -- The bot whose worker controls it, if any.
    bot_id text,
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

    -- Typed provenance for delegated sub-agent children. The complete
    -- SessionOrigin document is retained while the two queried ids are
    -- denormalized for root- and parent-scoped listing/limits. These are
    -- historical facts, not ownership, so they intentionally have no foreign
    -- keys to sessions.
    origin_json jsonb,
    origin_root_session_id text,
    origin_parent_session_id text,

    PRIMARY KEY (universe_id, session_id),

    -- Deleting a clone's source clears its lineage without clearing its universe.
    CONSTRAINT sessions_source_session_id_fkey
        FOREIGN KEY (universe_id, source_session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE SET NULL (source_session_id),

    CONSTRAINT sessions_session_id_format
        CHECK (session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_head_seq_positive
        CHECK (head_seq IS NULL OR head_seq > 0),
    CONSTRAINT sessions_lifecycle_status_known
        CHECK (lifecycle_status IN ('new', 'open', 'closed')),
    CONSTRAINT sessions_closed_at_seq_positive
        CHECK (closed_at_seq IS NULL OR closed_at_seq > 0),
    CONSTRAINT sessions_closed_projection_consistent
        CHECK (
            (lifecycle_status = 'closed' AND closed_at_seq IS NOT NULL)
            OR (lifecycle_status <> 'closed' AND closed_at_seq IS NULL)
        ),
    CONSTRAINT sessions_closed_seq_within_head
        CHECK (closed_at_seq IS NULL OR closed_at_seq <= head_seq),
    CONSTRAINT sessions_source_seq_nonnegative
        CHECK (source_seq IS NULL OR source_seq >= 0),
    CONSTRAINT sessions_source_seq_requires_source
        CHECK (source_seq IS NULL OR source_session_id IS NOT NULL),
    CONSTRAINT sessions_source_not_self
        CHECK (source_session_id IS NULL OR source_session_id <> session_id),
    CONSTRAINT sessions_origin_shape
        CHECK (
            (
                origin_json IS NULL
                AND origin_root_session_id IS NULL
                AND origin_parent_session_id IS NULL
            )
            OR (
                jsonb_typeof(origin_json) = 'object'
                AND origin_root_session_id IS NOT NULL
                AND origin_root_session_id <> ''
                AND origin_parent_session_id IS NOT NULL
                AND origin_parent_session_id <> ''
            )
        ),
    CONSTRAINT sessions_closed_at_pair
        CHECK ((closed_at_ms IS NULL) = (closed_at_seq IS NULL)),
    CONSTRAINT sessions_closed_at_ms_nonnegative
        CHECK (closed_at_ms IS NULL OR closed_at_ms >= 0),
    CONSTRAINT sessions_retention_root_format
        CHECK (retention_root_session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_delete_after_close_positive
        CHECK (delete_after_close_ms IS NULL OR delete_after_close_ms > 0),
    CONSTRAINT sessions_retention_policy_on_root
        CHECK (
            retention_root_session_id = session_id
            OR delete_after_close_ms IS NULL
        ),
    CONSTRAINT sessions_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT sessions_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT sessions_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT sessions_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS sessions_source_session_id_idx
    ON sessions (universe_id, source_session_id)
    WHERE source_session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS sessions_origin_root_idx
    ON sessions (universe_id, origin_root_session_id)
    WHERE origin_root_session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS sessions_origin_parent_idx
    ON sessions (universe_id, origin_parent_session_id)
    WHERE origin_parent_session_id IS NOT NULL;

-- Keyset paging for session listings: newest activity first.
CREATE INDEX IF NOT EXISTS sessions_updated_at_idx
    ON sessions (universe_id, updated_at_ms DESC, session_id DESC);

CREATE INDEX IF NOT EXISTS sessions_lifecycle_updated_at_idx
    ON sessions (universe_id, lifecycle_status, updated_at_ms DESC, session_id DESC);

CREATE INDEX IF NOT EXISTS sessions_retention_root_idx
    ON sessions (universe_id, retention_root_session_id, session_id);
CREATE INDEX IF NOT EXISTS sessions_retention_due_idx
    ON sessions (universe_id, delete_at_ms, session_id)
    WHERE lifecycle_status = 'closed' AND delete_at_ms IS NOT NULL;

CREATE INDEX IF NOT EXISTS sessions_metadata_idx
    ON sessions USING gin (metadata_json jsonb_path_ops);

-- What a session is doing now, for lists: a row only while a run is working
-- or waiting for approvals, written with the events that change it. Idle
-- sessions have no row; the event log remains authoritative.
CREATE TABLE IF NOT EXISTS session_activity (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    activity text NOT NULL CHECK (activity IN ('working', 'waiting')),
    since_ms bigint NOT NULL CHECK (since_ms >= 0),
    PRIMARY KEY (universe_id, session_id),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE
);

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
    created_at_ms bigint NOT NULL,
    touched_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, digest),

    CONSTRAINT cas_blobs_digest_format
        CHECK (digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT cas_blobs_byte_len_nonnegative
        CHECK (byte_len >= 0),
    CONSTRAINT cas_blobs_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT cas_blobs_touched_after_created
        CHECK (touched_at_ms >= created_at_ms),
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

-- The sweep considers the oldest untouched blobs after its grace period.
CREATE INDEX IF NOT EXISTS cas_blobs_touched_at_idx
    ON cas_blobs (universe_id, touched_at_ms, digest);

-- Derived by the session store in the append transaction. One row per
-- session and blob, regardless of how many events reference it. The FK
-- serializes attachment against collection, including uncommitted appends.
CREATE TABLE IF NOT EXISTS cas_session_roots (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    digest text NOT NULL,
    PRIMARY KEY (universe_id, session_id, digest),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    -- Check at commit so whole-universe cascades can remove holders first.
    FOREIGN KEY (universe_id, digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX IF NOT EXISTS cas_session_roots_digest_idx
    ON cas_session_roots (universe_id, digest);

-- Disposable, advance-only pointers to CAS-backed session reducer state.
-- The append-only event log remains the sole durable authority.

CREATE TABLE IF NOT EXISTS session_checkpoints (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    through_seq bigint NOT NULL,
    format_version integer NOT NULL,
    state_digest text NOT NULL,
    lineage_source_session_id text,
    lineage_source_seq bigint,
    byte_len bigint NOT NULL,
    created_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, session_id),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    FOREIGN KEY (universe_id, state_digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE RESTRICT,

    CONSTRAINT session_checkpoints_through_seq_positive CHECK (through_seq > 0),
    CONSTRAINT session_checkpoints_format_version_positive CHECK (format_version > 0),
    CONSTRAINT session_checkpoints_byte_len_nonnegative CHECK (byte_len >= 0),
    CONSTRAINT session_checkpoints_created_at_ms_nonnegative CHECK (created_at_ms >= 0),
    -- Clones record a source session with no source sequence (fresh log);
    -- only a sequence without a session is malformed.
    CONSTRAINT session_checkpoints_lineage_pair CHECK (
        NOT (lineage_source_session_id IS NULL AND lineage_source_seq IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS session_checkpoints_state_digest_idx
    ON session_checkpoints (universe_id, state_digest);

COMMENT ON TABLE session_checkpoints IS
    'Disposable advance-only pointers to CAS-backed reducer state; session_events remains authoritative.';

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
    'One row per Lightspeed session; head_seq is updated transactionally with event appends. source_session_id/source_seq record clone/fork content lineage; origin_json records delegated-child provenance.';
COMMENT ON COLUMN sessions.origin_json IS
    'Serialized SessionOrigin of a sub-agent child (kind, parent, parent run, root, depth, invocation, pinned profile revision, effective limits); null for roots.';
COMMENT ON COLUMN sessions.origin_root_session_id IS
    'Denormalized from origin_json: the lineage root; root-scoped limits count every session naming this root.';
COMMENT ON COLUMN sessions.origin_parent_session_id IS
    'Denormalized from origin_json: the delegating parent session.';
COMMENT ON TABLE session_events IS
    'Append-only stored session entries as canonical JSONB with generated query columns.';
COMMENT ON TABLE cas_blobs IS
    'Universe-scoped CAS catalog keyed by sha256 digest; small payloads inline, large payloads external.';
COMMENT ON TABLE cas_blob_edges IS
    'Required parent-child reachability edges for nested CAS formats; recorded by their writers.';
COMMENT ON COLUMN sessions.metadata_json IS
    'Descriptive key/value metadata; never routing, authority, or selection.';
COMMENT ON COLUMN cas_blobs.created_at_ms IS
    'Unix milliseconds of the first put of this content in the universe.';
COMMENT ON COLUMN cas_blobs.touched_at_ms IS
    'Unix milliseconds of the most recent put or input admission; collection grace counts from here.';
COMMENT ON TABLE cas_session_roots IS
    'Distinct blob references derived transactionally from a session log; removed with the session.';
