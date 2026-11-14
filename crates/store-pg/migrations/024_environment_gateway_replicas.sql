CREATE TABLE runtime_instances (
    instance_id uuid PRIMARY KEY,
    roles text[] NOT NULL,
    peer_url text,
    started_at_ms bigint NOT NULL CHECK (started_at_ms >= 0),
    lease_expires_at_ms bigint NOT NULL CHECK (lease_expires_at_ms >= started_at_ms)
);

CREATE INDEX runtime_instances_lease_idx ON runtime_instances (lease_expires_at_ms);

ALTER TABLE environments
    ADD COLUMN owner_instance_id uuid,
    ADD COLUMN control_connection_id text,
    ADD CONSTRAINT environments_connection_owner_pair CHECK (
        (owner_instance_id IS NULL) = (control_connection_id IS NULL)
    ),
    ADD CONSTRAINT environments_connection_owner_kind CHECK (
        owner_instance_id IS NULL OR source_kind = 'registered'
    );

CREATE INDEX environments_owner_instance_idx ON environments (owner_instance_id)
    WHERE owner_instance_id IS NOT NULL;

-- Existing connections have no process identity. The migration is the one
-- deployment boundary that invalidates them; starting a replica never does.
UPDATE environments SET connected = false,
    connection_changed_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
    updated_at_ms = GREATEST(updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
WHERE source_kind = 'registered' AND connected;
