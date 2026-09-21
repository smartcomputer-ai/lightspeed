-- Canonical deployment identity and access records. Credentials remain in the
-- auth registry; these tables contain no credential values or login sessions.
CREATE TABLE IF NOT EXISTS access_principals (
    principal_id uuid PRIMARY KEY CHECK (principal_id <> '00000000-0000-0000-0000-000000000000'),
    kind text NOT NULL CHECK (kind IN ('user', 'service')),
    status text NOT NULL CHECK (status IN ('active', 'disabled')),
    display_name text NOT NULL CHECK (length(btrim(display_name)) > 0 AND octet_length(display_name) <= 256),
    -- Retain management provenance when its universe is deleted. This is not
    -- an authority grant; role/capability rows cascade away with the universe.
    management_universe_id uuid,
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0),
    CHECK (kind = 'service' OR management_universe_id IS NULL)
);

CREATE TABLE IF NOT EXISTS access_groups (
    group_id uuid PRIMARY KEY CHECK (group_id <> '00000000-0000-0000-0000-000000000000'),
    display_name text NOT NULL CHECK (length(btrim(display_name)) > 0 AND octet_length(display_name) <= 256),
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0)
);

CREATE TABLE IF NOT EXISTS access_memberships (
    group_id uuid NOT NULL REFERENCES access_groups (group_id),
    principal_id uuid NOT NULL REFERENCES access_principals (principal_id),
    PRIMARY KEY (group_id, principal_id)
);
CREATE INDEX IF NOT EXISTS access_memberships_principal_idx ON access_memberships (principal_id);

CREATE TABLE IF NOT EXISTS access_role_assignments (
    universe_id uuid REFERENCES universes (universe_id) ON DELETE CASCADE,
    principal_id uuid REFERENCES access_principals (principal_id),
    group_id uuid REFERENCES access_groups (group_id),
    role text NOT NULL,
    CHECK ((principal_id IS NULL) <> (group_id IS NULL)),
    CHECK ((universe_id IS NULL AND role = 'deployment_admin') OR
           (universe_id IS NOT NULL AND role IN ('viewer', 'contributor', 'operator', 'admin')))
);
CREATE UNIQUE INDEX IF NOT EXISTS access_roles_principal_idx ON access_role_assignments
    (COALESCE(universe_id, '00000000-0000-0000-0000-000000000000'::uuid), principal_id, role)
    WHERE principal_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS access_roles_group_idx ON access_role_assignments
    (COALESCE(universe_id, '00000000-0000-0000-0000-000000000000'::uuid), group_id, role)
    WHERE group_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS access_capabilities (
    universe_id uuid REFERENCES universes (universe_id) ON DELETE CASCADE,
    principal_id uuid NOT NULL REFERENCES access_principals (principal_id),
    capability text NOT NULL CHECK (capability IN
        ('assert_user', 'lease_credentials', 'admit_channel_inbound', 'discover_channel_accounts', 'manage_identity')),
    CHECK (capability = 'assert_user' OR
           (universe_id IS NULL AND capability IN ('discover_channel_accounts', 'manage_identity')) OR
           (universe_id IS NOT NULL AND capability IN ('lease_credentials', 'admit_channel_inbound')))
);
CREATE UNIQUE INDEX IF NOT EXISTS access_capabilities_idx ON access_capabilities
    (COALESCE(universe_id, '00000000-0000-0000-0000-000000000000'::uuid), principal_id, capability);

-- Serializes identity writes and advances with each committed policy change.
-- Bootstrap is a durable one-shot marker, not "no active admin currently".
CREATE TABLE IF NOT EXISTS access_policy (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    bootstrap_principal_id uuid REFERENCES access_principals (principal_id)
);
INSERT INTO access_policy (singleton) VALUES (true) ON CONFLICT DO NOTHING;

-- No foreign keys to mutable identities, universes or session content: audit
-- must survive deletion/offboarding. Only access change metadata is recorded.
CREATE TABLE IF NOT EXISTS access_audit (
    revision bigint PRIMARY KEY CHECK (revision > 0),
    actor_id uuid,
    occurred_at_ms bigint NOT NULL CHECK (occurred_at_ms >= 0),
    event jsonb NOT NULL
);
