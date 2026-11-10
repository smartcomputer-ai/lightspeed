-- Custom request headers: a static credential holding a set of HTTP headers,
-- and the MCP auth policies that send it. Model providers cannot send custom
-- headers, so servers using them must be executed by Lightspeed.
ALTER TABLE auth_grants
    DROP CONSTRAINT auth_grants_provider_kind_known,
    ADD CONSTRAINT auth_grants_provider_kind_known
        CHECK (
            provider_kind IN (
                'static_bearer',
                'static_headers',
                'mcp_oauth',
                'github_app',
                'custom_oauth',
                'model_api_key',
                'model_oauth'
            )
        );

ALTER TABLE mcp_servers
    DROP CONSTRAINT mcp_servers_auth_policy_known,
    ADD CONSTRAINT mcp_servers_auth_policy_known
        CHECK (
            auth_policy IN (
                'none',
                'optional_bearer',
                'required_bearer',
                'optional_headers',
                'required_headers',
                'optional_oauth',
                'required_oauth'
            )
        ),
    ADD CONSTRAINT mcp_servers_headers_need_runtime
        CHECK (
            auth_policy NOT IN ('optional_headers', 'required_headers')
            OR execution = 'runtime'
        );
