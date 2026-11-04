-- 'native' meant the provider's own shape everywhere except here, where it meant
-- Lightspeed runs the call, so the value becomes 'runtime'. Nothing is rewritten: a
-- database holding 'native' rows fails these checks and has to be purged, as this
-- breaking release already requires.
ALTER TABLE mcp_servers
    DROP CONSTRAINT mcp_servers_execution_known,
    DROP CONSTRAINT mcp_servers_exposure_matches_execution,
    ALTER COLUMN execution SET DEFAULT 'runtime',
    ADD CONSTRAINT mcp_servers_execution_known
        CHECK (execution IN ('provider', 'runtime')),
    ADD CONSTRAINT mcp_servers_exposure_matches_execution
        CHECK (execution = 'runtime' OR exposure = 'inject');

-- The flag says a lifecycle controller owns the session, not that a declaration
-- created it: a session declaring only tools has it false.
ALTER TABLE sessions RENAME COLUMN managed TO lifecycle_controlled;
