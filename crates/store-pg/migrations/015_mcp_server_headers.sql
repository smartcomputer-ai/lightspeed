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
