-- Remote MCP servers, execution policy, and per-server request headers.

CREATE TABLE mcp_servers (
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    server_id text NOT NULL,
    display_name text,
    server_url text NOT NULL,
    transport text NOT NULL DEFAULT 'streamable_http',
    default_server_label text NOT NULL,
    description text,
    allowed_tools text[],
    approval_default text NOT NULL DEFAULT 'never',
    defer_loading_default boolean,
    execution text NOT NULL DEFAULT 'runtime',
    exposure text NOT NULL DEFAULT 'inject',
    allow_private_network boolean NOT NULL DEFAULT false,
    auth_policy text NOT NULL DEFAULT 'none',
    auth_metadata_json jsonb NOT NULL DEFAULT '{}',
    auth_grant_id text,
    status text NOT NULL DEFAULT 'active',
    revision bigint NOT NULL,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, server_id),

    CONSTRAINT mcp_servers_auth_grant_fk
        FOREIGN KEY (universe_id, auth_grant_id)
        REFERENCES credentials_grants (universe_id, grant_id) ON DELETE RESTRICT,

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
        CHECK (transport IN ('streamable_http')),
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
        CHECK (approval_default IN ('always', 'never')),
    CONSTRAINT mcp_servers_execution_known
        CHECK (execution IN ('provider', 'runtime')),
    CONSTRAINT mcp_servers_exposure_known
        CHECK (exposure IN ('inject', 'search')),
    CONSTRAINT mcp_servers_exposure_matches_execution
        CHECK (execution = 'runtime' OR exposure = 'inject'),
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
    CONSTRAINT mcp_servers_no_auth_grant_without_policy
        CHECK (auth_policy <> 'none' OR auth_grant_id IS NULL),
    CONSTRAINT mcp_servers_status_known
        CHECK (status IN ('active', 'needs_auth_config', 'unverified', 'disabled')),
    CONSTRAINT mcp_servers_revision_positive
        CHECK (revision > 0),
    CONSTRAINT mcp_servers_created_at_ms_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT mcp_servers_updated_at_ms_nonnegative
        CHECK (updated_at_ms >= 0),
    CONSTRAINT mcp_servers_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX mcp_servers_status_idx
    ON mcp_servers (universe_id, status, server_id);

CREATE INDEX mcp_servers_default_label_idx
    ON mcp_servers (universe_id, default_server_label);

CREATE INDEX mcp_servers_auth_policy_idx
    ON mcp_servers (universe_id, auth_policy, server_id);

CREATE INDEX mcp_servers_auth_grant_idx
    ON mcp_servers (universe_id, auth_grant_id)
    WHERE auth_grant_id IS NOT NULL;

COMMENT ON TABLE mcp_servers IS
    'Universe-scoped remote MCP server catalog; session-visible links are materialized into engine tool-set events.';
COMMENT ON COLUMN mcp_servers.server_id IS
    'Stable universe-scoped MCP server id used by API/CLI control-plane operations.';
COMMENT ON COLUMN mcp_servers.server_url IS
    'Remote MCP endpoint URL. Must not contain credentials; runtime auth is resolved through auth handles.';
COMMENT ON COLUMN mcp_servers.default_server_label IS
    'Default provider-facing MCP server label copied into RemoteMcpToolSpec unless overridden at session link time.';
COMMENT ON COLUMN mcp_servers.allowed_tools IS
    'Optional provider-side MCP tool allowlist. NULL means no catalog-level allowlist.';
COMMENT ON COLUMN mcp_servers.auth_policy IS
    'Non-secret MCP auth requirement hint. Generic credentials, grants, and token refresh are owned by the credential registry.';
COMMENT ON COLUMN mcp_servers.auth_metadata_json IS
    'Non-secret MCP auth metadata such as OAuth resource, scopes, protected resource metadata URL, or authorization server URL.';
COMMENT ON COLUMN mcp_servers.auth_grant_id IS
    'Universe credential grant selected by this configured MCP server; sessions never override this binding.';

-- Extra request headers of a configured MCP server, one row per header. A row
-- holds either a plain value or the credential grant whose token is sent raw. Grant
-- ids are a column rather than JSON so a credential in use by a header cannot
-- be deleted. The grant reference is deferrable but checked immediately, so
-- only a universe delete (which defers its constraints) can remove grants and
-- headers in either cascade order.
CREATE TABLE mcp_server_headers (
    universe_id uuid NOT NULL,
    server_id text NOT NULL,
    name text NOT NULL,
    literal_value text,
    grant_id text,

    PRIMARY KEY (universe_id, server_id, name),
    FOREIGN KEY (universe_id, server_id)
        REFERENCES mcp_servers (universe_id, server_id) ON DELETE CASCADE,
    CONSTRAINT mcp_server_headers_grant_fk
        FOREIGN KEY (universe_id, grant_id)
        REFERENCES credentials_grants (universe_id, grant_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT mcp_server_headers_one_source
        CHECK ((literal_value IS NULL) <> (grant_id IS NULL)),
    CONSTRAINT mcp_server_headers_name_lowercase
        CHECK (name <> '' AND name = lower(name)),
    -- These header names conventionally carry secrets.
    CONSTRAINT mcp_server_headers_secret_names_reference_a_credential
        CHECK (name NOT IN ('api-key', 'proxy-authorization', 'x-api-key') OR grant_id IS NOT NULL)
);

CREATE INDEX mcp_server_headers_grant_idx
    ON mcp_server_headers (universe_id, grant_id)
    WHERE grant_id IS NOT NULL;

COMMENT ON TABLE mcp_server_headers IS
    'Extra request headers sent to a configured MCP server; independent of its auth policy, which owns Authorization.';
COMMENT ON COLUMN mcp_server_headers.literal_value IS
    'Plain, non-secret header value returned by reads.';
COMMENT ON COLUMN mcp_server_headers.grant_id IS
    'Universe credential grant whose token is resolved per request and sent raw; the token never enters this table.';
