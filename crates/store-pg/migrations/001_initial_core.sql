-- Universes, session logs and projections, and content-addressed storage.

CREATE TABLE universes (
    universe_id uuid PRIMARY KEY,
    -- A deployment-unique URL label, distinct from a UUID.
    handle text NOT NULL,
    display_name text CHECK (display_name IS NULL OR char_length(display_name) BETWEEN 1 AND 120),
    metadata_json jsonb NOT NULL DEFAULT '{}'::jsonb,

    -- Every insert path stamps creation time, including ensure_universe.
    created_at_ms bigint NOT NULL
        DEFAULT ((extract(epoch FROM now()) * 1000)::bigint),
    protected boolean NOT NULL DEFAULT false,
    expires_at_ms bigint,
    deleting_at_ms bigint,
    deletion_json jsonb,

    CONSTRAINT universes_handle_format CHECK (
        handle ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
        AND handle !~ '^[0-9a-f]{32}$'
        AND handle !~ '^[0-9a-f]{8}-([0-9a-f]{4}-){3}[0-9a-f]{12}$'
    ),
    CONSTRAINT universes_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT universes_protection_expiry
        CHECK (NOT protected OR expires_at_ms IS NULL),
    CONSTRAINT universes_positive_expiry
        CHECK (expires_at_ms IS NULL OR expires_at_ms > 0),
    CONSTRAINT universes_protected_deletion
        CHECK (NOT protected OR deleting_at_ms IS NULL),
    CONSTRAINT universes_deletion_state
        CHECK ((deleting_at_ms IS NULL) = (deletion_json IS NULL))
);

-- Beginning deletion releases the handle. The UUID stays reserved.
CREATE UNIQUE INDEX universes_handle_key
    ON universes (handle)
    WHERE deleting_at_ms IS NULL;

CREATE INDEX universes_metadata_idx ON universes USING gin (metadata_json jsonb_path_ops);

CREATE INDEX universes_expiry_idx
    ON universes (expires_at_ms, universe_id)
    WHERE expires_at_ms IS NOT NULL AND deleting_at_ms IS NULL;

COMMENT ON TABLE universes IS
    'Tenant/project/workspace boundary; sessions and CAS are shared within one universe.';

-- Deletion receipts outlive the universe and retain its original identity.
CREATE TABLE universes_deleted (
    universe_id uuid PRIMARY KEY,
    handle text NOT NULL,
    display_name text,
    metadata_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at_ms bigint NOT NULL,
    deleting_at_ms bigint NOT NULL,
    deleted_at_ms bigint NOT NULL,
    requested_by jsonb NOT NULL,
    summary jsonb NOT NULL,
    CONSTRAINT universes_deleted_metadata_object CHECK (jsonb_typeof(metadata_json) = 'object')
);

CREATE INDEX universes_deleted_newest
    ON universes_deleted (deleted_at_ms DESC, universe_id DESC);

CREATE TABLE sessions (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    session_id text NOT NULL,
    display_name text,
    metadata_json jsonb NOT NULL DEFAULT '{}',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    -- Event-log head and catalog revision. Catalog fields can change between events.
    head_seq bigint,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision >= 0),

    -- Attribution and audience are recorded on roots. Delegated children
    -- inherit them, and a bot's session tree is shared with the universe.
    created_by jsonb,
    visibility text CHECK (visibility IN ('universe', 'restricted')),
    -- The owning bot of this root session, if any.
    bot_id text,

    -- Projected lifecycle state, updated in the event append transaction.
    lifecycle_status text NOT NULL DEFAULT 'new',
    lifecycle_controlled boolean NOT NULL DEFAULT false,
    closed_at_seq bigint,
    closed_at_ms bigint,
    -- Soft deletion is recorded separately from lifecycle events.
    deleted_at_ms bigint,

    -- History forks and delegated children share their owner's retention root;
    -- config-only clones are independent roots. Only roots carry a policy.
    retention_root_session_id text NOT NULL,
    delete_after_close_ms bigint,
    delete_at_ms bigint GENERATED ALWAYS AS
        (closed_at_ms + delete_after_close_ms) STORED,

    -- Content lineage for clones and forks. A clone starts its own log at 1
    -- with a NULL source_seq. A fork reads the source's first source_seq
    -- events by reference and appends from source_seq + 1.
    source_session_id text,
    source_seq bigint,

    -- Delegation provenance. The root and parent ids support scoped queries
    -- and can outlive their referenced sessions.
    origin_json jsonb,
    origin_root_session_id text,
    origin_parent_session_id text,

    PRIMARY KEY (universe_id, session_id),

    CONSTRAINT sessions_session_id_format
        CHECK (session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_metadata_object
        CHECK (jsonb_typeof(metadata_json) = 'object'),
    CONSTRAINT sessions_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT sessions_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT sessions_updated_after_created
        CHECK (updated_at_ms >= created_at_ms),
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
    CONSTRAINT sessions_closed_at_pair
        CHECK ((closed_at_ms IS NULL) = (closed_at_seq IS NULL)),
    CONSTRAINT sessions_closed_at_ms_nonnegative
        CHECK (closed_at_ms IS NULL OR closed_at_ms >= 0),
    CONSTRAINT sessions_deleted_closed
        CHECK (deleted_at_ms IS NULL OR lifecycle_status = 'closed'),

    CONSTRAINT sessions_retention_root_format
        CHECK (retention_root_session_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT sessions_delete_after_close_positive
        CHECK (delete_after_close_ms IS NULL OR delete_after_close_ms > 0),
    CONSTRAINT sessions_retention_policy_on_root
        CHECK (
            retention_root_session_id = session_id
            OR delete_after_close_ms IS NULL
        ),

    -- Deleting a clone's source clears its lineage without clearing its universe.
    CONSTRAINT sessions_source_session_id_fkey
        FOREIGN KEY (universe_id, source_session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE SET NULL (source_session_id),
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
        )
);

CREATE INDEX sessions_source_session_id_idx
    ON sessions (universe_id, source_session_id)
    WHERE source_session_id IS NOT NULL;

CREATE INDEX sessions_origin_root_idx
    ON sessions (universe_id, origin_root_session_id)
    WHERE origin_root_session_id IS NOT NULL;

CREATE INDEX sessions_origin_parent_idx
    ON sessions (universe_id, origin_parent_session_id)
    WHERE origin_parent_session_id IS NOT NULL;

-- Keyset paging for session listings: newest activity first.
CREATE INDEX sessions_updated_at_idx
    ON sessions (universe_id, updated_at_ms DESC, session_id DESC);

CREATE INDEX sessions_lifecycle_updated_at_idx
    ON sessions (universe_id, lifecycle_status, updated_at_ms DESC, session_id DESC);

CREATE INDEX sessions_retention_root_idx
    ON sessions (universe_id, retention_root_session_id, session_id);
CREATE INDEX sessions_retention_due_idx
    ON sessions (universe_id, delete_at_ms, session_id)
    WHERE lifecycle_status = 'closed' AND delete_at_ms IS NOT NULL;

CREATE INDEX sessions_metadata_idx
    ON sessions USING gin (metadata_json jsonb_path_ops);

COMMENT ON TABLE sessions IS
    'One row per Lightspeed session; head_seq is updated transactionally with event appends. source_session_id/source_seq record clone/fork content lineage; origin_json records delegated-child provenance.';
COMMENT ON COLUMN sessions.origin_json IS
    'Serialized SessionOrigin of a sub-agent child (kind, parent, parent run, root, depth, invocation, pinned profile revision, effective limits); null for roots.';
COMMENT ON COLUMN sessions.origin_root_session_id IS
    'Denormalized from origin_json: the lineage root; root-scoped limits count every session naming this root.';
COMMENT ON COLUMN sessions.origin_parent_session_id IS
    'Denormalized from origin_json: the delegating parent session.';
COMMENT ON COLUMN sessions.metadata_json IS
    'Descriptive key/value metadata; never routing, authority, or selection.';

CREATE TABLE session_events (
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

CREATE INDEX session_events_event_kind_idx
    ON session_events (universe_id, event_kind);

COMMENT ON TABLE session_events IS
    'Append-only stored session entries as canonical JSONB with generated query columns.';

-- Materialized session summary and current activity for catalog reads.
CREATE TABLE session_summary (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    activity text NOT NULL DEFAULT 'idle' CHECK (activity IN ('idle', 'working', 'waiting')),
    phase text,
    since_ms bigint CHECK (since_ms >= 0),
    summary_json jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(summary_json) = 'object'),
    PRIMARY KEY (universe_id, session_id),
    FOREIGN KEY (universe_id, session_id)
        REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    CHECK ((activity = 'idle') = (since_ms IS NULL))
);

CREATE INDEX session_summary_active_idx
    ON session_summary (universe_id, session_id)
    WHERE activity <> 'idle';

-- Define sessions before cas_blobs so a universe delete cascades through
-- session checkpoints before deleting the blobs they reference.
CREATE TABLE cas_blobs (
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

CREATE UNIQUE INDEX cas_blobs_blob_ref_idx
    ON cas_blobs (universe_id, blob_ref);

CREATE UNIQUE INDEX cas_blobs_object_key_idx
    ON cas_blobs (object_key)
    WHERE object_key IS NOT NULL;

-- The sweep considers the oldest untouched blobs after its grace period.
CREATE INDEX cas_blobs_touched_at_idx
    ON cas_blobs (universe_id, touched_at_ms, digest);

COMMENT ON TABLE cas_blobs IS
    'Universe-scoped CAS catalog keyed by sha256 digest; small payloads inline, large payloads external.';
COMMENT ON COLUMN cas_blobs.created_at_ms IS
    'Unix milliseconds of the first put of this content in the universe.';
COMMENT ON COLUMN cas_blobs.touched_at_ms IS
    'Unix milliseconds of the most recent put or input admission; collection grace counts from here.';

-- This projection depends on both sessions and cas_blobs. The event log
-- remains authoritative when a checkpoint is discarded.
CREATE TABLE session_checkpoints (
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

CREATE INDEX session_checkpoints_state_digest_idx
    ON session_checkpoints (universe_id, state_digest);

COMMENT ON TABLE session_checkpoints IS
    'Disposable advance-only pointers to CAS-backed reducer state; session_events remains authoritative.';

-- Derived by the session store in the append transaction. One row per
-- session and blob, regardless of how many events reference it. The FK
-- serializes attachment against collection, including uncommitted appends.
CREATE TABLE cas_session_roots (
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
CREATE INDEX cas_session_roots_digest_idx
    ON cas_session_roots (universe_id, digest);

COMMENT ON TABLE cas_session_roots IS
    'Distinct blob references derived transactionally from a session log; removed with the session.';

CREATE TABLE cas_blob_edges (
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

CREATE INDEX cas_blob_edges_child_digest_idx
    ON cas_blob_edges (universe_id, child_digest);

COMMENT ON TABLE cas_blob_edges IS
    'Required parent-child reachability edges for nested CAS formats; recorded by their writers.';

-- Where the blob sweep stopped in each universe, so a restart or a new leader
-- carries on rather than walking the oldest rows again. A position and not a
-- holder: the digest has no foreign key to cas_blobs, and the blob it names may
-- be deleted while the row stands.
CREATE TABLE cas_sweep_cursors (
    universe_id uuid PRIMARY KEY
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    touched_at_ms bigint NOT NULL,
    digest text NOT NULL
);

-- Renewable leases for runtime replicas.
CREATE TABLE runtime_instances (
    instance_id uuid PRIMARY KEY,
    roles text[] NOT NULL,
    peer_url text,
    started_at_ms bigint NOT NULL CHECK (started_at_ms >= 0),
    lease_expires_at_ms bigint NOT NULL CHECK (lease_expires_at_ms >= started_at_ms)
);

CREATE INDEX runtime_instances_lease_idx ON runtime_instances (lease_expires_at_ms);
