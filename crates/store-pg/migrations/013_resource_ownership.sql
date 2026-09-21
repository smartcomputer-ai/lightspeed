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

-- Admission facts, not success claims. No request bodies or credential values.
-- Deliberately independent of content lifetimes and identity deletion.
CREATE TABLE access_action_audit (
    audit_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    universe_id uuid NOT NULL,
    actor jsonb NOT NULL,
    authentication jsonb,
    action text NOT NULL,
    resource jsonb,
    policy_revision bigint,
    admitted boolean NOT NULL,
    occurred_at_ms bigint NOT NULL CHECK (occurred_at_ms >= 0)
);
