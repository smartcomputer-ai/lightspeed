-- P71 G4: channel-neutral delivery outbox.

CREATE TABLE IF NOT EXISTS messaging_outbox (
    universe_id uuid NOT NULL REFERENCES universes (universe_id) ON DELETE CASCADE,
    seq bigint GENERATED ALWAYS AS IDENTITY,
    outbox_id text NOT NULL,
    session_id text NOT NULL,
    run_id bigint,
    origin text NOT NULL,
    payload_json jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    attempts integer NOT NULL DEFAULT 0,
    channel_message_id text,
    error text,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, outbox_id),
    CONSTRAINT messaging_outbox_status_check
        CHECK (status IN ('pending', 'delivered', 'failed')),
    CONSTRAINT messaging_outbox_origin_check
        CHECK (origin IN ('tool_call', 'final_text', 'trigger'))
);

CREATE UNIQUE INDEX IF NOT EXISTS messaging_outbox_seq_idx
    ON messaging_outbox (universe_id, seq);

CREATE INDEX IF NOT EXISTS messaging_outbox_pending_idx
    ON messaging_outbox (universe_id, seq)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS messaging_outbox_session_created_idx
    ON messaging_outbox (universe_id, session_id, created_at_ms);
