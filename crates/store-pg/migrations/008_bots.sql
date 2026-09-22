-- Bots: the bot registry, its triggers, and the per-bot event log.
--
-- Design notes:
-- - Bot decisions live in the controller's Temporal history; these tables
--   are the read model. `bot_events` is the bot's numbered log of what
--   arrived and what it sent, each row with a write-once `outcome`; trigger
--   incidents (`disabled_reason`, `last_filter_error`) are trigger state.
--   A filter miss is never stored.
-- - A bot is addressed by its authored, immutable `bot_id`; there is no
--   surrogate key. `display_name` lives in the document and may change.
-- - Every table splits the same way: identity columns, the operator's
--   `document_json` (replaced whole with an expected `revision`; exactly
--   what the client put, nothing runtime-owned), and runtime-owned
--   columns the operator never writes.
-- - Event rows keep `trigger_id` without a foreign key: a trigger may be
--   deleted while its history stays readable.
-- - Trigger secrets (webhook URL token, pairing code) are capability
--   tokens Lightspeed mints for the trigger, not credentials it holds;
--   they sit beside the document in `secrets_json` and are shown only to
--   managing principals. Credentials proper (HMAC secrets, poll auth) are
--   grant references inside the document.
-- - Chat provider accounts and conversation pairings live in
--   `009_channels.sql`.

-- ═══════════════════════════════════════════════════════════════════════════
-- bots
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS bots (
    -- ── Identity ───────────────────────────────────────────────────────────
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    -- Authored, immutable id (`ci-triage`): the name models, `bot_emit`,
    -- and the controller workflow id `{universe}/bot-{bot_id}` use.
    bot_id text NOT NULL,

    -- ── The operator's document ────────────────────────────────────────────
    -- Bumped by every put, close, and runtime disable; a put must carry it.
    revision bigint NOT NULL,
    -- BotDocument: displayName, description, profileId, brief, runsPerDay,
    -- breaker {fires, windowMs}, routedSessionTtlMs, the capability grants
    -- selfConfig and emit, enabled.
    document_json jsonb NOT NULL,

    -- ── Runtime-owned ──────────────────────────────────────────────────────
    -- The #N counter: the highest event seq allocated so far, advanced
    -- atomically at admission.
    event_seq bigint NOT NULL DEFAULT 0,
    -- Terminal close marker set once by bots/close; a closed bot refuses
    -- events and cannot be re-enabled.
    closed_at_ms bigint,
    -- Session ids the controller force-closed on the way out, recorded so
    -- bots/delete can erase them.
    closed_sessions_json jsonb NOT NULL DEFAULT '[]',
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, bot_id),

    CONSTRAINT bots_bot_id_format
        CHECK (bot_id ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT bots_revision_positive
        CHECK (revision > 0),
    CONSTRAINT bots_document_object
        CHECK (jsonb_typeof(document_json) = 'object'),
    CONSTRAINT bots_event_seq_nonnegative
        CHECK (event_seq >= 0),
    CONSTRAINT bots_closed_at_ms_nonnegative
        CHECK (closed_at_ms IS NULL OR closed_at_ms >= 0),
    CONSTRAINT bots_closed_sessions_array
        CHECK (jsonb_typeof(closed_sessions_json) = 'array'),
    CONSTRAINT bots_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT bots_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

COMMENT ON TABLE bots IS
    'Universe-scoped bot registry: the operator document, the #N event counter, and the terminal close marker.';
COMMENT ON COLUMN bots.bot_id IS
    'Authored, immutable bot id; the name models and Temporal identities address the bot by.';
COMMENT ON COLUMN bots.revision IS
    'Document revision, bumped by every put, close, and runtime disable; checked by put-with-expected-revision.';
COMMENT ON COLUMN bots.document_json IS
    'Serialized BotDocument (display name, description, profile, brief, budgets, capability grants, enabled).';
COMMENT ON COLUMN bots.event_seq IS
    'Highest #N allocated so far; incremented atomically at admission.';
COMMENT ON COLUMN bots.closed_at_ms IS
    'Terminal close marker, set once by bots/close; a closed bot refuses events and cannot be re-enabled.';
COMMENT ON COLUMN bots.closed_sessions_json IS
    'Session ids the controller force-closed on the way out, recorded so bots/delete can erase them.';
COMMENT ON COLUMN bots.created_at_ms IS
    'Creation time in Unix milliseconds.';
COMMENT ON COLUMN bots.updated_at_ms IS
    'Last document write in Unix milliseconds.';

-- ═══════════════════════════════════════════════════════════════════════════
-- bot_triggers
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS bot_triggers (
    -- ── Identity ───────────────────────────────────────────────────────────
    universe_id uuid NOT NULL,
    bot_id text NOT NULL,
    -- Authored trigger id, unique per bot; part of the fire workflow and
    -- Schedule ids.
    trigger_id text NOT NULL,
    -- Copy of document_json->>'kind' (CHECK-enforced) so listing by kind is
    -- indexed: schedule | webhook | poll | bot (the inbox) | chat.
    kind text NOT NULL,

    -- ── The operator's document ────────────────────────────────────────────
    -- Bumped by every put and runtime disable.
    revision bigint NOT NULL,
    -- BotTriggerDocument: the kind-tagged spec (schedule {cron|atMs,
    -- timezone, summary} / webhook {verification, preset} / poll {source,
    -- intervalMs, items, cursor} / bot {from} / chat {accountId, matchScope,
    -- activation, access, pairing, priority}) plus the cross-kind policy:
    -- filter (CEL), route (bot | perKey | perEvent), coalesce, deliver,
    -- sessionTtlMs, enabled.
    document_json jsonb NOT NULL,

    -- ── Runtime-owned ──────────────────────────────────────────────────────
    -- Server-minted capability tokens: {webhookToken} for webhook triggers
    -- (the URL path token), {pairingCode} for chat triggers with pairing:
    -- code. Kept out of the document so it round-trips verbatim; shown only
    -- to managing principals.
    secrets_json jsonb NOT NULL DEFAULT '{}',
    -- Why the runtime disabled the trigger (breaker | poll_failed |
    -- one_shot | operator | bot_closed); NULL while enabled. A put that
    -- enables the trigger clears it. Set and cleared together with
    -- disabled_at_ms.
    disabled_reason text,
    disabled_at_ms bigint,
    -- Last runtime failure of the CEL filter (the event was refused,
    -- fail-closed); cleared by the next match. Set and cleared together
    -- with last_filter_error_at_ms.
    last_filter_error text,
    last_filter_error_at_ms bigint,
    -- Poll triggers only: PollCursorState {ids (seen ids, capped) |
    -- watermark, consecutiveFailures, baselinedAtMs, lastPolledAtMs}.
    -- Absent until the baseline poll; reset by a spec edit.
    cursor_json jsonb,
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, bot_id, trigger_id),
    CONSTRAINT bot_triggers_bot_fk FOREIGN KEY (universe_id, bot_id)
        REFERENCES bots (universe_id, bot_id) ON DELETE CASCADE,

    CONSTRAINT bot_triggers_trigger_id_format
        CHECK (trigger_id ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT bot_triggers_kind_known
        CHECK (kind IN ('schedule', 'webhook', 'poll', 'bot', 'chat')),
    CONSTRAINT bot_triggers_kind_matches_document
        CHECK (document_json->>'kind' = kind),
    CONSTRAINT bot_triggers_revision_positive
        CHECK (revision > 0),
    CONSTRAINT bot_triggers_document_object
        CHECK (jsonb_typeof(document_json) = 'object'),
    CONSTRAINT bot_triggers_secrets_object
        CHECK (jsonb_typeof(secrets_json) = 'object'),
    CONSTRAINT bot_triggers_disabled_reason_known
        CHECK (
            disabled_reason IS NULL
            OR disabled_reason IN ('breaker', 'poll_failed', 'one_shot', 'operator', 'bot_closed')
        ),
    CONSTRAINT bot_triggers_disabled_pair
        CHECK ((disabled_reason IS NULL) = (disabled_at_ms IS NULL)),
    CONSTRAINT bot_triggers_disabled_at_ms_nonnegative
        CHECK (disabled_at_ms IS NULL OR disabled_at_ms >= 0),
    CONSTRAINT bot_triggers_filter_error_pair
        CHECK ((last_filter_error IS NULL) = (last_filter_error_at_ms IS NULL)),
    CONSTRAINT bot_triggers_filter_error_at_ms_nonnegative
        CHECK (last_filter_error_at_ms IS NULL OR last_filter_error_at_ms >= 0),
    CONSTRAINT bot_triggers_cursor_object
        CHECK (cursor_json IS NULL OR jsonb_typeof(cursor_json) = 'object'),
    CONSTRAINT bot_triggers_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT bot_triggers_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX IF NOT EXISTS bot_triggers_kind_idx
    ON bot_triggers (universe_id, kind, bot_id, trigger_id);

-- At most one inbox (`kind = 'bot'`) per bot: the inbox owns the bot's
-- filter, route, coalesce, and delivery policy for addressed events.
CREATE UNIQUE INDEX IF NOT EXISTS bot_triggers_inbox_unique_idx
    ON bot_triggers (universe_id, bot_id)
    WHERE kind = 'bot';

COMMENT ON TABLE bot_triggers IS
    'Triggers of a bot (schedule, webhook, poll, inbox, chat): the revisioned document, server-minted secrets, and runtime incidents.';
COMMENT ON COLUMN bot_triggers.trigger_id IS
    'Authored trigger id, unique per bot.';
COMMENT ON COLUMN bot_triggers.kind IS
    'Trigger kind copied from the document spec for indexed listing; schedule|webhook|poll|bot|chat.';
COMMENT ON COLUMN bot_triggers.revision IS
    'Document revision, bumped by every put and runtime disable.';
COMMENT ON COLUMN bot_triggers.document_json IS
    'Serialized BotTriggerDocument: the kind-specific spec plus filter, route, coalesce, deliver, sessionTtlMs, enabled.';
COMMENT ON COLUMN bot_triggers.secrets_json IS
    'Server-minted capability tokens (webhookToken, pairingCode); shown only to managing principals.';
COMMENT ON COLUMN bot_triggers.disabled_reason IS
    'Why the runtime disabled the trigger (breaker|poll_failed|one_shot|operator|bot_closed); cleared when a put enables it again.';
COMMENT ON COLUMN bot_triggers.disabled_at_ms IS
    'When the runtime disabled the trigger, in Unix milliseconds.';
COMMENT ON COLUMN bot_triggers.last_filter_error IS
    'Last runtime failure of the CEL filter (the event was refused); cleared by the next match.';
COMMENT ON COLUMN bot_triggers.last_filter_error_at_ms IS
    'When the last filter failure happened, in Unix milliseconds.';
COMMENT ON COLUMN bot_triggers.cursor_json IS
    'Poll triggers: the advancing PollCursorState; absent until the baseline poll, reset by a spec edit.';
COMMENT ON COLUMN bot_triggers.created_at_ms IS
    'Creation time in Unix milliseconds.';
COMMENT ON COLUMN bot_triggers.updated_at_ms IS
    'Last document write in Unix milliseconds.';

-- ═══════════════════════════════════════════════════════════════════════════
-- bot_events
-- ═══════════════════════════════════════════════════════════════════════════
--
-- The bot's numbered log: what arrived and what it sent. A row is written
-- once at admission with everything the controller needs to deliver it,
-- and touched once more when the delivery finishes (the outcome group).
-- Filter misses are never stored. The source of an event is `trigger_id`
-- (a webhook, schedule, poll, or chat trigger) or `sender_bot_id` (a bot);
-- the full envelope, including the source's own data and headers, is the
-- CAS document at `document_ref`.

CREATE TABLE IF NOT EXISTS bot_events (
    -- ── Identity and order ─────────────────────────────────────────────────
    universe_id uuid NOT NULL,
    bot_id text NOT NULL,
    -- Dedupe identity: the provider delivery id where known, otherwise
    -- derived at admission. A re-delivered event keeps its #N.
    event_id text NOT NULL,
    -- Per-bot #N allocated from bots.event_seq; the only event handle
    -- shown to models and humans.
    seq bigint NOT NULL,

    -- ── What arrived ───────────────────────────────────────────────────────
    -- Admitting trigger; no foreign key so history outlives the trigger.
    -- NULL for an operator admit.
    trigger_id text,
    -- Event kind as authored by the source (github.push, schedule.fire,
    -- chat.message, bot.reply).
    kind text NOT NULL,
    -- One-line human summary rendered in the log and the roster.
    summary text NOT NULL,
    -- When the source says it happened, in Unix milliseconds.
    occurred_at_ms bigint NOT NULL,
    -- Admission time in Unix milliseconds; log order and the rate windows
    -- are keyed on it.
    received_at_ms bigint NOT NULL,
    -- CAS ref of the full BotEventDocument envelope (kind, source, data,
    -- headers, links, sender, hops); replays re-admit it by ref.
    document_ref text NOT NULL,

    -- ── Delivery plan, computed at admission ───────────────────────────────
    -- CAS ref of the model-facing rendering; pins what the session saw
    -- even after the renderer changes. NULL only for archived rows.
    prompt_ref text,
    -- RoutedSession {sessionId, label, ttl} chosen by the trigger's route;
    -- NULL means the bot's main session.
    session_json jsonb,
    -- Prepared BotEventMedia attachments appended to the run input; bytes
    -- live in the CAS.
    media_json jsonb NOT NULL DEFAULT '[]',

    -- ── Federation ─────────────────────────────────────────────────────────
    -- Sending bot for bot-originated events (self and addressed emits,
    -- receipts); counted against the sender rate cap.
    sender_bot_id text,
    -- Bot-to-bot hops from the world; an emit in response carries hops + 1
    -- and MAX_BOT_HOPS cuts the chain.
    hops integer NOT NULL DEFAULT 0,
    -- Public correlation of a receipt: BotEventReplyRef {bot, seq}, the
    -- asked event at the answering bot.
    in_reply_to_json jsonb,

    -- ── Receiver ───────────────────────────────────────────────────────────
    -- Private: who admitted the event and hears back when the delivery
    -- finishes. EventReceiver tagged by kind — `workflow` (a chat
    -- conversation: workflowId, workflowKind, the receipt token, and the
    -- toolsRef of the receiver-bound tools the routed session is created
    -- with) or `bot` (the asking bot of a `bot_emit { reply: true }` and
    -- its logical session, sent a bot.reply receipt). NULL when nobody
    -- listens.
    receiver_json jsonb,

    -- ── Outcome, written once when the delivery finishes ───────────────────
    -- The model's decision (handled | deferred | ignored | blocked) or the
    -- system's (unresolved | run_failed | steered | appended | archived);
    -- NULL while pending.
    outcome text,
    -- Free-text detail recorded with the outcome (the model's reason).
    outcome_detail text,
    -- Run that resolved the event, when one was started.
    run_id text,
    -- When the outcome was written, in Unix milliseconds.
    resolved_at_ms bigint,

    PRIMARY KEY (universe_id, bot_id, event_id),
    CONSTRAINT bot_events_bot_fk FOREIGN KEY (universe_id, bot_id)
        REFERENCES bots (universe_id, bot_id) ON DELETE CASCADE,
    CONSTRAINT bot_events_seq_unique UNIQUE (universe_id, bot_id, seq),

    CONSTRAINT bot_events_event_id_not_empty
        CHECK (event_id <> ''),
    CONSTRAINT bot_events_seq_nonnegative
        CHECK (seq >= 0),
    CONSTRAINT bot_events_trigger_id_format
        CHECK (trigger_id IS NULL OR trigger_id ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT bot_events_kind_not_empty
        CHECK (kind <> ''),
    CONSTRAINT bot_events_occurred_nonnegative
        CHECK (occurred_at_ms >= 0),
    CONSTRAINT bot_events_received_nonnegative
        CHECK (received_at_ms >= 0),
    CONSTRAINT bot_events_document_ref_format
        CHECK (document_ref ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT bot_events_prompt_ref_format
        CHECK (prompt_ref IS NULL OR prompt_ref ~ '^sha256:[0-9a-f]{64}$'),
    CONSTRAINT bot_events_session_object
        CHECK (session_json IS NULL OR jsonb_typeof(session_json) = 'object'),
    CONSTRAINT bot_events_media_array
        CHECK (jsonb_typeof(media_json) = 'array'),
    CONSTRAINT bot_events_sender_bot_id_format
        CHECK (sender_bot_id IS NULL OR sender_bot_id ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT bot_events_hops_nonnegative
        CHECK (hops >= 0),
    CONSTRAINT bot_events_in_reply_to_object
        CHECK (in_reply_to_json IS NULL OR jsonb_typeof(in_reply_to_json) = 'object'),
    CONSTRAINT bot_events_receiver_object
        CHECK (receiver_json IS NULL OR jsonb_typeof(receiver_json) = 'object'),
    CONSTRAINT bot_events_receiver_kind_known
        CHECK (receiver_json IS NULL OR receiver_json->>'kind' IN ('workflow', 'bot')),
    CONSTRAINT bot_events_outcome_known
        CHECK (
            outcome IS NULL
            OR outcome IN (
                'handled', 'deferred', 'ignored', 'blocked', 'unresolved',
                'run_failed', 'steered', 'appended', 'archived'
            )
        ),
    CONSTRAINT bot_events_resolved_nonnegative
        CHECK (resolved_at_ms IS NULL OR resolved_at_ms >= 0),
    CONSTRAINT bot_events_outcome_pair
        CHECK ((outcome IS NULL) = (resolved_at_ms IS NULL))
);

-- The log: newest first per bot.
CREATE INDEX IF NOT EXISTS bot_events_log_idx
    ON bot_events (universe_id, bot_id, received_at_ms DESC, seq DESC);

-- Per-trigger flood breaker window.
CREATE INDEX IF NOT EXISTS bot_events_trigger_rate_idx
    ON bot_events (universe_id, bot_id, trigger_id, received_at_ms);

-- Per-sender emit rate cap, across every receiving bot in the universe.
CREATE INDEX IF NOT EXISTS bot_events_sender_rate_idx
    ON bot_events (universe_id, sender_bot_id, received_at_ms)
    WHERE sender_bot_id IS NOT NULL;

-- Pending count on the roster.
CREATE INDEX IF NOT EXISTS bot_events_pending_idx
    ON bot_events (universe_id, bot_id)
    WHERE outcome IS NULL;

COMMENT ON TABLE bot_events IS
    'Per-bot numbered event log: what arrived and what the bot sent, each with a write-once delivery outcome. Filter misses are never stored.';
COMMENT ON COLUMN bot_events.event_id IS
    'Dedupe identity: the provider delivery id where known, otherwise derived at admission.';
COMMENT ON COLUMN bot_events.seq IS
    'Per-bot #N, allocated from bots.event_seq; the only event handle shown to models and humans.';
COMMENT ON COLUMN bot_events.trigger_id IS
    'Admitting trigger; no foreign key so history outlives the trigger. NULL for an operator admit.';
COMMENT ON COLUMN bot_events.kind IS
    'Event kind as authored by the source (e.g. github.push, schedule.fire, chat.message, bot.reply).';
COMMENT ON COLUMN bot_events.summary IS
    'One-line human summary rendered in the log and the roster.';
COMMENT ON COLUMN bot_events.occurred_at_ms IS
    'When the source says the event happened, in Unix milliseconds.';
COMMENT ON COLUMN bot_events.received_at_ms IS
    'Admission time in Unix milliseconds; the log and rate windows are keyed on it.';
COMMENT ON COLUMN bot_events.document_ref IS
    'CAS ref of the full BotEventDocument envelope.';
COMMENT ON COLUMN bot_events.prompt_ref IS
    'CAS ref of the model-facing rendering delivered to sessions; NULL only for archived rows.';
COMMENT ON COLUMN bot_events.session_json IS
    'RoutedSession the event was admitted to (session id, label, ttl); NULL means the main session.';
COMMENT ON COLUMN bot_events.media_json IS
    'Prepared BotEventMedia attachments appended to the run input; bytes live in the CAS.';
COMMENT ON COLUMN bot_events.sender_bot_id IS
    'Sending bot for bot-originated events (self or addressed); counted against the sender rate cap.';
COMMENT ON COLUMN bot_events.hops IS
    'Federation hop count of the event; bounded by MAX_BOT_HOPS at admission.';
COMMENT ON COLUMN bot_events.in_reply_to_json IS
    'Public BotEventReplyRef correlation of a receipt: the asked event #N at the answering bot.';
-- Derived by the bot store in the same transaction as event insertion.
-- Covers documents, prompts, media, and receiver tool declarations.
CREATE TABLE IF NOT EXISTS cas_bot_event_roots (
    universe_id uuid NOT NULL,
    bot_id text NOT NULL,
    event_id text NOT NULL,
    digest text NOT NULL,
    -- Every bot event reference is placed by the runtime, so it is content.
    origin text NOT NULL DEFAULT 'content' CHECK (origin IN ('content', 'scan')),
    PRIMARY KEY (universe_id, bot_id, event_id, digest),
    FOREIGN KEY (universe_id, bot_id, event_id)
        REFERENCES bot_events (universe_id, bot_id, event_id) ON DELETE CASCADE,
    -- Check at commit so whole-universe cascades can remove holders first.
    FOREIGN KEY (universe_id, digest)
        REFERENCES cas_blobs (universe_id, digest) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX IF NOT EXISTS cas_bot_event_roots_digest_idx
    ON cas_bot_event_roots (universe_id, digest);

COMMENT ON COLUMN bot_events.receiver_json IS
    'Private EventReceiver: the admitting workflow (receipt endpoint, token, receiver-bound tools) or the asking bot (bot.reply route). NULL when nobody listens.';
COMMENT ON COLUMN bot_events.outcome IS
    'Write-once delivery outcome: the model''s decision (handled|deferred|ignored|blocked) or the system''s; NULL while pending.';
COMMENT ON COLUMN bot_events.outcome_detail IS
    'Free-text detail recorded with the outcome.';
COMMENT ON COLUMN bot_events.run_id IS
    'Run that resolved the event, when one was started.';
COMMENT ON COLUMN bot_events.resolved_at_ms IS
    'When the outcome was written, in Unix milliseconds.';
