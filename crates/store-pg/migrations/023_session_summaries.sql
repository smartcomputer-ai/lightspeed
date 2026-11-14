ALTER TABLE sessions ADD COLUMN revision bigint NOT NULL DEFAULT 1 CHECK (revision >= 0);

CREATE TABLE session_summary (
    universe_id uuid NOT NULL,
    session_id text NOT NULL,
    activity text NOT NULL DEFAULT 'idle' CHECK (activity IN ('idle', 'working', 'waiting')),
    phase text,
    since_ms bigint CHECK (since_ms >= 0),
    summary_json jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(summary_json) = 'object'),
    PRIMARY KEY (universe_id, session_id),
    FOREIGN KEY (universe_id, session_id) REFERENCES sessions (universe_id, session_id) ON DELETE CASCADE,
    CHECK ((activity = 'idle') = (since_ms IS NULL))
);

INSERT INTO session_summary (universe_id, session_id, activity, since_ms)
SELECT universe_id, session_id, activity, since_ms FROM session_activity;
DROP TABLE session_activity;

CREATE INDEX session_summary_active_idx ON session_summary (universe_id, session_id)
    WHERE activity <> 'idle';

ALTER TABLE bots ADD COLUMN controller_status_json jsonb;
ALTER TABLE bots ADD COLUMN controller_status_at_ms bigint;
