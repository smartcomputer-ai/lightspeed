-- No foreign keys to mutable identities, universes or session content: audit
-- must survive deletion/offboarding. Only access change metadata is recorded.
CREATE TABLE access_audit_changes (
    revision bigint PRIMARY KEY CHECK (revision > 0),
    actor_id uuid,
    occurred_at_ms bigint NOT NULL CHECK (occurred_at_ms >= 0),
    event jsonb NOT NULL
);

-- One row per audited operation or post-authentication denial, written once at
-- the API boundary. `policy_revision` joins the change log above; a null
-- universe is deployment scope.
CREATE TABLE access_audit_events (
    audit_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at_ms bigint NOT NULL CHECK (occurred_at_ms >= 0),
    method text NOT NULL,
    outcome text NOT NULL CHECK (outcome IN ('succeeded', 'failed', 'denied')),
    error_kind text,
    authenticated_principal_id uuid,
    acting_principal_id uuid,
    credential text,
    universe_id uuid,
    target jsonb,
    policy_revision bigint,
    -- A decision of the request relied on read_private_content. Reads are
    -- otherwise quiet; every privileged read has a row.
    privileged boolean NOT NULL DEFAULT false
);
CREATE INDEX access_audit_events_time_idx ON access_audit_events (occurred_at_ms);
CREATE INDEX access_audit_events_actor_idx ON access_audit_events (acting_principal_id, occurred_at_ms);
CREATE INDEX access_audit_events_universe_idx ON access_audit_events (universe_id, occurred_at_ms);
