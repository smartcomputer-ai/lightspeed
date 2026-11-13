-- Extra request headers of a model provider's endpoint, one row per header. A
-- row holds either a plain value or the auth grant whose token is sent raw.
-- Grant ids are a column rather than JSON so a credential in use by a header
-- cannot be deleted. The grant reference is deferrable but checked immediately,
-- so only a universe delete (which defers its constraints) can remove grants
-- and headers in either cascade order. The shape is that of mcp_server_headers:
-- both tables hold the same configured request headers, each for its own owner.
CREATE TABLE IF NOT EXISTS auth_provider_headers (
    universe_id uuid NOT NULL,
    provider_id text NOT NULL,
    name text NOT NULL,
    literal_value text,
    grant_id text,

    PRIMARY KEY (universe_id, provider_id, name),
    FOREIGN KEY (universe_id, provider_id)
        REFERENCES auth_providers (universe_id, provider_id) ON DELETE CASCADE,
    CONSTRAINT auth_provider_headers_grant_fk
        FOREIGN KEY (universe_id, grant_id)
        REFERENCES auth_grants (universe_id, grant_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT auth_provider_headers_one_source
        CHECK ((literal_value IS NULL) <> (grant_id IS NULL)),
    CONSTRAINT auth_provider_headers_name_lowercase
        CHECK (name <> '' AND name = lower(name)),
    -- These names carry a secret by convention, and a plain value is returned
    -- to every reader of the provider.
    CONSTRAINT auth_provider_headers_secret_names_reference_a_credential
        CHECK (
            name NOT IN ('api-key', 'proxy-authorization', 'x-api-key')
            OR grant_id IS NOT NULL
        )
);

CREATE INDEX IF NOT EXISTS auth_provider_headers_grant_idx
    ON auth_provider_headers (universe_id, grant_id)
    WHERE grant_id IS NOT NULL;

COMMENT ON TABLE auth_provider_headers IS
    'Extra request headers sent to a model provider''s endpoint; independent of the provider''s own credential, which owns Authorization.';
COMMENT ON COLUMN auth_provider_headers.literal_value IS
    'Plain, non-secret header value returned by reads.';
COMMENT ON COLUMN auth_provider_headers.grant_id IS
    'Universe auth grant whose token is resolved per request and sent raw; the token never enters this table.';

-- The same names take only a credential on an MCP server, whose plain header
-- values are likewise returned to every reader of the server.
ALTER TABLE mcp_server_headers
    ADD CONSTRAINT mcp_server_headers_secret_names_reference_a_credential
        CHECK (
            name NOT IN ('api-key', 'proxy-authorization', 'x-api-key')
            OR grant_id IS NOT NULL
        );
