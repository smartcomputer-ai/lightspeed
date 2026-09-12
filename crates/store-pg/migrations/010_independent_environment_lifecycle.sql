-- Environments have independent lifecycles. Existing resources remain intact;
-- sessions no longer create them or trigger their closure.
DROP INDEX IF EXISTS environments_origin_session_idx;
DROP INDEX IF EXISTS environments_close_with_session_idx;
ALTER TABLE environments
    DROP CONSTRAINT IF EXISTS environments_origin_session_shape,
    DROP COLUMN origin_session_id,
    DROP COLUMN origin_profile_id,
    DROP COLUMN origin_close_with_session;
