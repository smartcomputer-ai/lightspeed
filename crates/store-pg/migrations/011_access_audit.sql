-- No foreign keys to mutable identities, universes or session content: audit
-- must survive deletion/offboarding. Only access change metadata is recorded.
CREATE TABLE access_audit_changes (
    revision bigint PRIMARY KEY CHECK (revision > 0),
    actor_id uuid,
    occurred_at_ms bigint NOT NULL CHECK (occurred_at_ms >= 0),
    event jsonb NOT NULL
);

-- Security decisions are independent of content and principal lifetimes.
CREATE TABLE access_audit_events (
    audit_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    attempt_id uuid NOT NULL,
    method text,
    identity jsonb NOT NULL,
    actor jsonb,
    scope jsonb,
    target jsonb,
    policy_revision bigint,
    stage text NOT NULL CHECK (stage IN ('authentication', 'admission', 'completion', 'delivery')),
    outcome text NOT NULL CHECK (outcome IN ('allowed', 'denied', 'succeeded', 'failed')),
    error_kind text,
    occurred_at_ms bigint NOT NULL CHECK (occurred_at_ms >= 0)
);
CREATE INDEX access_audit_events_attempt ON access_audit_events(attempt_id);

