-- Anchor of every governed resource: immutable facts reserved before the
-- resource exists, so a creation race can never transfer it. The anchor is
-- released with the resource, so an id belongs to the universe, not to
-- whoever used it first.
CREATE TABLE access_resources (
    universe_id uuid NOT NULL REFERENCES universes(universe_id) ON DELETE CASCADE,
    resource_kind text NOT NULL CHECK (resource_kind IN
        ('session', 'bot', 'profile', 'workspace', 'environment', 'mcp_server')),
    resource_id text NOT NULL,
    -- Creation attribution and the immediate admitted controller.
    created_by jsonb NOT NULL,
    controller jsonb NOT NULL,
    -- The root whose policy governs this resource: itself for a root, the
    -- parent's root for a delegated child, the bot's root for a bot's session.
    -- Copied at reservation so a decision reads one row and never walks a chain.
    audience_root_kind text NOT NULL CHECK (audience_root_kind IN
        ('session', 'bot', 'profile', 'workspace', 'environment', 'mcp_server')),
    audience_root_id text NOT NULL,
    -- The bot whose worker controls this resource, if any.
    bot_id text,
    -- The principal the root's work runs as, and whether that is the
    -- universe's execution service or the owner itself. Copied from the root
    -- at reservation; absent for kinds that run nothing: profiles, and the
    -- workspaces, environments and MCP servers that sessions use.
    run_as_principal_id uuid REFERENCES access_principals(principal_id),
    execution_kind text CHECK (execution_kind IN ('service', 'personal')),
    created_at_ms bigint NOT NULL CHECK (created_at_ms >= 0),
    PRIMARY KEY (universe_id, resource_kind, resource_id),
    CHECK ((run_as_principal_id IS NULL) = (execution_kind IS NULL)),
    CHECK ((resource_kind IN ('session', 'bot')) = (run_as_principal_id IS NOT NULL))
);
CREATE INDEX access_resources_root_idx ON access_resources (universe_id, audience_root_kind, audience_root_id);

-- What can change about a root: its current owner and visibility. Resources
-- below a root have no policy of their own; a resource whose root has no
-- policy row is unreadable by everyone. The revision advances with every
-- replacement of the policy or its grants.
CREATE TABLE access_resource_policies (
    universe_id uuid NOT NULL,
    resource_kind text NOT NULL,
    resource_id text NOT NULL,
    owner_principal_id uuid NOT NULL REFERENCES access_principals(principal_id),
    visibility text NOT NULL CHECK (visibility IN ('universe', 'restricted')),
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_by jsonb NOT NULL,
    updated_at_ms bigint NOT NULL CHECK (updated_at_ms >= 0),
    PRIMARY KEY (universe_id, resource_kind, resource_id),
    FOREIGN KEY (universe_id, resource_kind, resource_id)
        REFERENCES access_resources(universe_id, resource_kind, resource_id) ON DELETE CASCADE
);

-- One permission for one principal or group on a root: `read` or `write` on
-- sessions and bots, `use` on workspaces, environments and MCP servers.
CREATE TABLE access_resource_grants (
    universe_id uuid NOT NULL,
    resource_kind text NOT NULL,
    resource_id text NOT NULL,
    subject_kind text NOT NULL CHECK (subject_kind IN ('principal', 'group')),
    subject_id uuid NOT NULL,
    permission text NOT NULL CHECK (permission IN ('read', 'write', 'use')),
    granted_by uuid NOT NULL REFERENCES access_principals(principal_id),
    granted_at_ms bigint NOT NULL CHECK (granted_at_ms >= 0),
    PRIMARY KEY (universe_id, resource_kind, resource_id, subject_kind, subject_id),
    FOREIGN KEY (universe_id, resource_kind, resource_id)
        REFERENCES access_resources(universe_id, resource_kind, resource_id) ON DELETE CASCADE
);
CREATE INDEX access_resource_grants_subject_idx ON access_resource_grants (subject_kind, subject_id);
