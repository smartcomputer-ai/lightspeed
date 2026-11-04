-- Where the blob sweep stopped in each universe, so a restart or a new leader
-- carries on rather than walking the oldest rows again. A position and not a
-- holder: the digest has no foreign key to cas_blobs, and the blob it names may
-- be deleted while the row stands.
CREATE TABLE cas_sweep_cursors (
    universe_id uuid PRIMARY KEY REFERENCES universes (universe_id) ON DELETE CASCADE,
    touched_at_ms bigint NOT NULL,
    digest text NOT NULL
);
