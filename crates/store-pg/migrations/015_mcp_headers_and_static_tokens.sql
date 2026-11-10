-- Extra request headers of a configured MCP server, one row per header. A row
-- holds either a plain value or the auth grant whose token is sent raw. Grant
-- ids are a column rather than JSON so a credential in use by a header cannot
-- be deleted. The grant reference is deferrable but checked immediately, so
-- only a universe delete (which defers its constraints) can remove grants and
-- headers in either cascade order.
CREATE TABLE IF NOT EXISTS mcp_server_headers (
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
        REFERENCES auth_grants (universe_id, grant_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT mcp_server_headers_one_source
        CHECK ((literal_value IS NULL) <> (grant_id IS NULL)),
    CONSTRAINT mcp_server_headers_name_lowercase
        CHECK (name <> '' AND name = lower(name))
);

CREATE INDEX IF NOT EXISTS mcp_server_headers_grant_idx
    ON mcp_server_headers (universe_id, grant_id)
    WHERE grant_id IS NOT NULL;

COMMENT ON TABLE mcp_server_headers IS
    'Extra request headers sent to a configured MCP server; independent of its auth policy, which owns Authorization.';
COMMENT ON COLUMN mcp_server_headers.literal_value IS
    'Plain, non-secret header value returned by reads.';
COMMENT ON COLUMN mcp_server_headers.grant_id IS
    'Universe auth grant whose token is resolved per request and sent raw; the token never enters this table.';

-- The pasted single-secret credential is a token sent under whatever header its
-- consumer chooses, not only `Authorization: Bearer`, so its provider kind is
-- 'static_token'. Existing rows are relabelled in place. Their sealed secrets keep
-- the old kind 'auth.static_bearer': a secret's kind is bound into the AAD that
-- seals its value and is read back from its own row, so it must not change, and
-- nothing compares it with the current constant. Replacing a token's value stores
-- it under the new kind.
ALTER TABLE auth_grants DROP CONSTRAINT auth_grants_provider_kind_known;
ALTER TABLE auth_providers DROP CONSTRAINT auth_providers_provider_kind_known;
UPDATE auth_grants SET provider_kind = 'static_token' WHERE provider_kind = 'static_bearer';
UPDATE auth_providers SET provider_kind = 'static_token' WHERE provider_kind = 'static_bearer';

ALTER TABLE auth_grants
    ADD CONSTRAINT auth_grants_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_token',
                'mcp_oauth',
                'github_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth'
            )
        );

ALTER TABLE auth_providers
    ADD CONSTRAINT auth_providers_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_token',
                'mcp_oauth',
                'github_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth',
                'model_endpoint'
            )
        );
