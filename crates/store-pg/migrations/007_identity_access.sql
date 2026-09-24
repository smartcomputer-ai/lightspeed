-- Canonical deployment identity, authorization and inbound API keys.
-- No login sessions, external identity mapping or plaintext credentials.
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
           (universe_id IS NOT NULL AND role IN ('viewer', 'contributor', 'operator', 'admin', 'executor')))
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
        ('assert_user', 'lease_credentials', 'admit_channel_inbound', 'discover_channel_accounts', 'manage_identity',
         'read_private_content')),
    CHECK (capability = 'assert_user' OR
           (universe_id IS NULL AND capability IN ('discover_channel_accounts', 'manage_identity')) OR
           (universe_id IS NOT NULL AND capability IN ('lease_credentials', 'admit_channel_inbound', 'read_private_content')))
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

-- Inbound API keys authenticate a canonical principal within a credential scope.
-- Only hashes and non-secret key references are persisted; plaintext is shown
-- once at issuance. A null universe selects deployment scope.
CREATE TABLE api_keys (
    key_hash text PRIMARY KEY CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    key_prefix text NOT NULL UNIQUE CHECK (key_prefix <> ''),
    universe_id uuid REFERENCES universes(universe_id) ON DELETE CASCADE,
    principal_id uuid NOT NULL REFERENCES access_principals(principal_id),
    created_by uuid NOT NULL REFERENCES access_principals(principal_id),
    display_name text,
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0),
    revoked_at_ms bigint CHECK (revoked_at_ms IS NULL OR revoked_at_ms >= created_at_ms),
    last_used_at_ms bigint
);
CREATE INDEX authenticated_keys_universe_idx ON api_keys(universe_id);


-- Every universe's work runs as its execution service unless a person runs
-- it as themselves, which an administrator must enable. The service
-- principal is created with the universe's first governed resource.
ALTER TABLE universes
    ADD COLUMN execution_principal_id uuid REFERENCES access_principals (principal_id),
    ADD COLUMN personal_execution_enabled boolean NOT NULL DEFAULT false;

-- Who minted an environment registration key: environments the key admits
-- are owned by that principal. A key without one registers nothing.
ALTER TABLE environment_registration_keys
    ADD COLUMN created_by_principal_id uuid REFERENCES access_principals (principal_id);
