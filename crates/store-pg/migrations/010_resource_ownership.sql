-- Admission reserves immutable control facts before asynchronous creation.
-- Keep reservations after deletion: a reused id must never acquire a new owner.
CREATE TABLE access_resource_ownership (
    universe_id uuid NOT NULL REFERENCES universes(universe_id) ON DELETE CASCADE,
    resource_kind text NOT NULL CHECK (resource_kind IN ('session', 'bot', 'profile')),
    resource_id text NOT NULL,
    created_by jsonb NOT NULL,
    controller jsonb NOT NULL,
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (universe_id, resource_kind, resource_id)
);
