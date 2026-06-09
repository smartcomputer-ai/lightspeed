-- Remote MCP server catalog for direct provider-hosted MCP.
--
-- Design notes:
-- - This migration intentionally creates only the MCP catalog table.
-- - Generic secrets, OAuth clients, grants, refresh state, and token leases
--   belong to P69 and must not be stored here.
-- - Session links are materialized into the event-sourced engine tool set;
--   there is no separate session_mcp_links table in this migration.
-- - The catalog stores non-secret MCP server configuration and auth policy
--   hints only. Runtime auth is referenced later through generic auth handles.

CREATE TABLE IF NOT EXISTS mcp_servers (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    server_id text NOT NULL,
    display_name text,
    server_url text NOT NULL,
    transport text NOT NULL DEFAULT 'auto',
    default_server_label text NOT NULL,
    description text,
    allowed_tools text[],
    approval_default text NOT NULL DEFAULT 'provider_default',
    defer_loading_default boolean,
    auth_policy text NOT NULL DEFAULT 'none',
    auth_metadata_json jsonb NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'active',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,
    inserted_at timestamptz NOT NULL DEFAULT now(),
    modified_at timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (universe_id, server_id),

    CONSTRAINT mcp_servers_server_id_format
        CHECK (server_id ~ '^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$'),
    CONSTRAINT mcp_servers_display_name_not_empty
        CHECK (display_name IS NULL OR display_name <> ''),
    CONSTRAINT mcp_servers_server_url_len
        CHECK (length(server_url) BETWEEN 1 AND 2048),
    CONSTRAINT mcp_servers_server_url_http
        CHECK (server_url ~* '^https?://'),
    CONSTRAINT mcp_servers_server_url_host_present
        CHECK (server_url ~* '^https?://[^/?#]+'),
    CONSTRAINT mcp_servers_server_url_no_credentials
        CHECK (server_url !~* '^[a-z][a-z0-9+.-]*://[^/?#]*@'),
    CONSTRAINT mcp_servers_server_url_no_fragment
        CHECK (position('#' IN server_url) = 0),
    CONSTRAINT mcp_servers_server_url_no_whitespace
        CHECK (server_url !~ '[[:space:][:cntrl:]]'),
    CONSTRAINT mcp_servers_transport_known
        CHECK (transport IN ('streamable_http', 'sse', 'auto')),
    CONSTRAINT mcp_servers_default_server_label_format
        CHECK (default_server_label ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$'),
    CONSTRAINT mcp_servers_description_not_empty
        CHECK (description IS NULL OR description <> ''),
    CONSTRAINT mcp_servers_allowed_tools_nonempty
        CHECK (allowed_tools IS NULL OR cardinality(allowed_tools) > 0),
    CONSTRAINT mcp_servers_allowed_tools_no_nulls
        CHECK (allowed_tools IS NULL OR array_position(allowed_tools, NULL::text) IS NULL),
    CONSTRAINT mcp_servers_allowed_tools_no_empty
        CHECK (allowed_tools IS NULL OR array_position(allowed_tools, '') IS NULL),
    CONSTRAINT mcp_servers_approval_default_known
        CHECK (approval_default IN ('provider_default', 'always', 'never')),
    CONSTRAINT mcp_servers_auth_policy_known
        CHECK (
            auth_policy IN (
                'none',
                'optional_bearer',
                'required_bearer',
                'optional_oauth',
                'required_oauth'
            )
        ),
    CONSTRAINT mcp_servers_auth_metadata_is_object
        CHECK (jsonb_typeof(auth_metadata_json) = 'object'),
    CONSTRAINT mcp_servers_status_known
        CHECK (status IN ('active', 'needs_auth_config', 'unverified', 'disabled')),
    CONSTRAINT mcp_servers_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT mcp_servers_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT mcp_servers_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS mcp_servers_status_idx
    ON mcp_servers (universe_id, status, server_id);

CREATE INDEX IF NOT EXISTS mcp_servers_default_label_idx
    ON mcp_servers (universe_id, default_server_label);

CREATE INDEX IF NOT EXISTS mcp_servers_auth_policy_idx
    ON mcp_servers (universe_id, auth_policy, server_id);

COMMENT ON TABLE mcp_servers IS
    'Universe-scoped remote MCP server catalog; session-visible links are materialized into engine tool-set events.';
COMMENT ON COLUMN mcp_servers.server_id IS
    'Stable universe-scoped MCP server id used by API/CLI control-plane operations.';
COMMENT ON COLUMN mcp_servers.server_url IS
    'Remote MCP endpoint URL. Must not contain credentials; runtime auth is resolved through P69 auth handles.';
COMMENT ON COLUMN mcp_servers.default_server_label IS
    'Default provider-facing MCP server label copied into RemoteMcpToolSpec unless overridden at session link time.';
COMMENT ON COLUMN mcp_servers.allowed_tools IS
    'Optional provider-side MCP tool allowlist. NULL means no catalog-level allowlist.';
COMMENT ON COLUMN mcp_servers.auth_policy IS
    'Non-secret MCP auth requirement hint. Generic credentials, grants, and token refresh are owned by P69.';
COMMENT ON COLUMN mcp_servers.auth_metadata_json IS
    'Non-secret MCP auth metadata such as OAuth resource, scopes, protected resource metadata URL, or authorization server URL.';
