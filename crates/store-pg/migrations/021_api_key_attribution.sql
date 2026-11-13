-- A request a key makes for itself is attributed to the key's id and the kind
-- of its scope: {"kind":"key","keyId":"ci-bot","scopeKind":"universe"}. It
-- used to be attributed to the display prefix of the key's secret,
-- {"kind":"key","prefix":"lsk_ab12cd34"}, which stops naming the key the
-- moment its secret is replaced. The runtime no longer reads that shape, so
-- every stored attribution of it is rewritten here.
--
-- A stored prefix is resolved through the key that holds it today. Prefixes
-- are unique among keys, and a revoked key keeps its row, so the only keys
-- that cannot be found are those whose secret was replaced after the record
-- was written. Those records keep what is known in the one form that still
-- reads: an internal attribution whose cause quotes the prefix. A match also
-- has to be possible: only a deployment key can have minted a key, and a
-- record inside a universe was made by a deployment key or a key of that
-- universe.

CREATE TEMPORARY TABLE api_key_attributions ON COMMIT DROP AS
SELECT
    key_prefix,
    universe_id,
    jsonb_build_object(
        'kind', 'key',
        'keyId', key_id,
        'scopeKind', CASE WHEN universe_id IS NULL THEN 'deployment' ELSE 'universe' END
    ) AS attribution
FROM api_keys;

-- Who minted a key.
UPDATE api_keys target
SET created_by = COALESCE(
    (
        SELECT named.attribution
        FROM api_key_attributions named
        WHERE named.key_prefix = target.created_by ->> 'prefix'
          AND named.universe_id IS NULL
    ),
    jsonb_build_object(
        'kind', 'internal',
        'component', 'migration',
        'cause', 'API key with secret prefix '
            || COALESCE(target.created_by ->> 'prefix', 'unknown')
            || ', replaced since'
    )
)
WHERE target.created_by ->> 'kind' = 'key'
  AND target.created_by -> 'keyId' IS NULL;

-- Who created a credential.
UPDATE auth_grants target
SET created_by = COALESCE(
    (
        SELECT named.attribution
        FROM api_key_attributions named
        WHERE named.key_prefix = target.created_by ->> 'prefix'
          AND (named.universe_id IS NULL OR named.universe_id = target.universe_id)
    ),
    jsonb_build_object(
        'kind', 'internal',
        'component', 'migration',
        'cause', 'API key with secret prefix '
            || COALESCE(target.created_by ->> 'prefix', 'unknown')
            || ', replaced since'
    )
)
WHERE target.created_by ->> 'kind' = 'key'
  AND target.created_by -> 'keyId' IS NULL;

-- Who started an OAuth authorization.
UPDATE auth_flows target
SET created_by = COALESCE(
    (
        SELECT named.attribution
        FROM api_key_attributions named
        WHERE named.key_prefix = target.created_by ->> 'prefix'
          AND (named.universe_id IS NULL OR named.universe_id = target.universe_id)
    ),
    jsonb_build_object(
        'kind', 'internal',
        'component', 'migration',
        'cause', 'API key with secret prefix '
            || COALESCE(target.created_by ->> 'prefix', 'unknown')
            || ', replaced since'
    )
)
WHERE target.created_by ->> 'kind' = 'key'
  AND target.created_by -> 'keyId' IS NULL;

-- Who started a session.
UPDATE sessions target
SET created_by = COALESCE(
    (
        SELECT named.attribution
        FROM api_key_attributions named
        WHERE named.key_prefix = target.created_by ->> 'prefix'
          AND (named.universe_id IS NULL OR named.universe_id = target.universe_id)
    ),
    jsonb_build_object(
        'kind', 'internal',
        'component', 'migration',
        'cause', 'API key with secret prefix '
            || COALESCE(target.created_by ->> 'prefix', 'unknown')
            || ', replaced since'
    )
)
WHERE target.created_by ->> 'kind' = 'key'
  AND target.created_by -> 'keyId' IS NULL;

-- Who created a bot.
UPDATE bots target
SET created_by = COALESCE(
    (
        SELECT named.attribution
        FROM api_key_attributions named
        WHERE named.key_prefix = target.created_by ->> 'prefix'
          AND (named.universe_id IS NULL OR named.universe_id = target.universe_id)
    ),
    jsonb_build_object(
        'kind', 'internal',
        'component', 'migration',
        'cause', 'API key with secret prefix '
            || COALESCE(target.created_by ->> 'prefix', 'unknown')
            || ', replaced since'
    )
)
WHERE target.created_by ->> 'kind' = 'key'
  AND target.created_by -> 'keyId' IS NULL;

-- Session events are the authoritative log, and an event that does not decode
-- makes its whole session unreadable. Five event kinds record who asked, each
-- at one place in its payload. Checkpoints are not rewritten: one that holds
-- the old shape is rejected when it is read and the session replays its log.
CREATE TEMPORARY TABLE attributed_session_events (
    event_kind text PRIMARY KEY,
    attribution_path text[] NOT NULL
) ON COMMIT DROP;

INSERT INTO attributed_session_events (event_kind, attribution_path) VALUES
    ('lightspeed.core.run.accepted',
        ARRAY['event', 'payload', 'run', 'accepted', 'requested_by']),
    ('lightspeed.core.run.steering_accepted',
        ARRAY['event', 'payload', 'run', 'steering_accepted', 'requested_by']),
    ('lightspeed.core.run.cancellation_requested',
        ARRAY['event', 'payload', 'run', 'cancellation_requested', 'requested_by']),
    ('lightspeed.core.run.queued_cancelled',
        ARRAY['event', 'payload', 'run', 'queued_cancelled', 'requested_by']),
    ('lightspeed.core.approval.decided',
        ARRAY['event', 'payload', 'approval', 'decided', 'decided_by']);

UPDATE session_events target
SET entry_json = jsonb_set(
    target.entry_json,
    attributed.attribution_path,
    COALESCE(
        (
            SELECT named.attribution
            FROM api_key_attributions named
            WHERE named.key_prefix
                    = target.entry_json #>> array_append(attributed.attribution_path, 'prefix')
              AND (named.universe_id IS NULL OR named.universe_id = target.universe_id)
        ),
        jsonb_build_object(
            'kind', 'internal',
            'component', 'migration',
            'cause', 'API key with secret prefix '
                || COALESCE(
                    target.entry_json #>> array_append(attributed.attribution_path, 'prefix'),
                    'unknown'
                )
                || ', replaced since'
        )
    )
)
FROM attributed_session_events attributed
WHERE target.event_kind = attributed.event_kind
  AND target.entry_json #>> array_append(attributed.attribution_path, 'kind') = 'key'
  AND target.entry_json #> array_append(attributed.attribution_path, 'keyId') IS NULL;

COMMENT ON COLUMN api_keys.key_prefix IS
    'Display prefix of the key''s current secret. It changes with the secret, so records name a key by key_id, never by this.';
