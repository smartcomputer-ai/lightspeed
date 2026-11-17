-- VFS catalog for immutable snapshots, mutable workspace heads, and mounts.
--
-- Design notes:
-- - VFS snapshot manifests are CAS-backed and point at core cas_blobs rows.
-- - Workspaces track mutable heads over immutable snapshot refs.
-- - Mounts expose snapshots or workspaces to a session-visible filesystem.

CREATE TABLE vfs_snapshots (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    digest text NOT NULL,
    snapshot_ref text GENERATED ALWAYS AS ('sha256:' || digest) STORED,
    source_json jsonb NOT NULL,
    display_name text,
    created_at_ms bigint NOT NULL,

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

CREATE UNIQUE INDEX vfs_snapshots_snapshot_ref_idx
    ON vfs_snapshots (universe_id, snapshot_ref);

CREATE TABLE vfs_workspaces (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    workspace_id text NOT NULL,
    display_name text,
    base_snapshot_digest text,
    head_snapshot_digest text NOT NULL,
    head_files bigint NOT NULL,
    head_bytes bigint NOT NULL,
    revision bigint NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

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
    CONSTRAINT vfs_workspaces_head_files_nonnegative
        CHECK (head_files >= 0),
    CONSTRAINT vfs_workspaces_head_bytes_nonnegative
        CHECK (head_bytes >= 0),
    CONSTRAINT vfs_workspaces_revision_nonnegative
        CHECK (revision >= 0),
    CONSTRAINT vfs_workspaces_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT vfs_workspaces_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT vfs_workspaces_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX vfs_workspaces_head_digest_idx
    ON vfs_workspaces (universe_id, head_snapshot_digest);

CREATE INDEX vfs_workspaces_base_digest_idx
    ON vfs_workspaces (universe_id, base_snapshot_digest);

COMMENT ON TABLE vfs_snapshots IS
    'Descriptive metadata for immutable CAS-backed VFS snapshot manifests.';
COMMENT ON TABLE vfs_workspaces IS
    'Mutable workspace heads pointing at immutable VFS snapshot refs.';
