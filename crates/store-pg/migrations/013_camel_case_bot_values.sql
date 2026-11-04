-- The bot event outcome and trigger disabled reason columns hold the public API's
-- values, which are camelCase across the API. Nothing is rewritten: a database
-- holding the earlier snake_case values fails these checks and has to be purged,
-- as this breaking release already requires.
ALTER TABLE bot_triggers
    DROP CONSTRAINT bot_triggers_disabled_reason_known,
    ADD CONSTRAINT bot_triggers_disabled_reason_known
        CHECK (
            disabled_reason IS NULL
            OR disabled_reason IN ('breaker', 'pollFailed', 'oneShot', 'operator', 'botClosed')
        );

COMMENT ON COLUMN bot_triggers.disabled_reason IS
    'Why the runtime disabled the trigger (breaker|pollFailed|oneShot|operator|botClosed); cleared when a put enables it again.';

ALTER TABLE bot_events
    DROP CONSTRAINT bot_events_outcome_known,
    ADD CONSTRAINT bot_events_outcome_known
        CHECK (
            outcome IS NULL
            OR outcome IN (
                'handled', 'deferred', 'ignored', 'blocked', 'unresolved',
                'runFailed', 'steered', 'appended', 'archived'
            )
        );
