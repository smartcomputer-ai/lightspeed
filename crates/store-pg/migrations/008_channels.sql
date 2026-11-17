-- Channels: chat provider accounts served by the connector host and
-- the conversation pairings that authorize a chat against a bot's chat
-- trigger.
--
-- Design notes:
-- - Accounts are universe resources with authored ids; the credential is a
--   grant reference in the document, never a token. One token serves one
--   universe — enforced deployment-wide by
--   `channel_accounts_provider_account_unique` — and the connector host
--   discovers accounts across universes.
-- - `provider` is an open, authored name (`telegram`, `whatsapp`,
--   `slack`, …): format-checked, never enumerated. Adding a channel type
--   is a connector concern, not a core schema change.
-- - Pairing is the routing authority: every bound conversation has a
--   pairing row (claimed by an open trigger's first contact, or by
--   pairing code), the paired trigger owns the chat while the row exists
--   (a disabled owner parks the chat, it is never rerouted), and deleting
--   the trigger or the pairing frees it. Rows cascade from both the chat
--   trigger and the account.

-- ═══════════════════════════════════════════════════════════════════════════
-- channel_accounts
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE channel_accounts (
    -- ── Identity ───────────────────────────────────────────────────────────
    universe_id uuid NOT NULL
        REFERENCES universes (universe_id) ON DELETE CASCADE,
    -- Authored account id, unique per universe; chat triggers point at it
    -- and the connector queue name derives from it.
    account_id text NOT NULL,
    -- Copy of the document's provider name for indexed listing; open
    -- vocabulary, format-checked only.
    provider text NOT NULL,
    -- Provider-native account identity (Telegram bot username or id,
    -- WhatsApp phone number); unique per provider across the deployment.
    provider_account_id text NOT NULL,

    -- ── The operator's document ────────────────────────────────────────────
    -- Bumped by every put.
    revision bigint NOT NULL,
    -- ChannelAccountDocument: provider, providerAccountId, displayName,
    -- credentialGrantId, settings, enabled. Never secret material.
    document_json jsonb NOT NULL,

    -- ── Runtime-owned ──────────────────────────────────────────────────────
    created_at_ms bigint NOT NULL,
    updated_at_ms bigint NOT NULL,

    PRIMARY KEY (universe_id, account_id),
    -- Deployment-wide: one provider account (one bot token, one number)
    -- belongs to exactly one universe. Two runners on one token would
    -- fight over the provider connection.
    CONSTRAINT channel_accounts_provider_account_unique
        UNIQUE (provider, provider_account_id),

    CONSTRAINT channel_accounts_account_id_format
        CHECK (account_id ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT channel_accounts_provider_format
        CHECK (provider ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT channel_accounts_provider_matches_document
        CHECK (document_json->>'provider' = provider),
    CONSTRAINT channel_accounts_provider_account_matches_document
        CHECK (document_json->>'providerAccountId' = provider_account_id),
    CONSTRAINT channel_accounts_provider_account_id_not_empty
        CHECK (provider_account_id <> ''),
    CONSTRAINT channel_accounts_revision_positive
        CHECK (revision > 0),
    CONSTRAINT channel_accounts_document_object
        CHECK (jsonb_typeof(document_json) = 'object'),
    CONSTRAINT channel_accounts_created_nonnegative
        CHECK (created_at_ms >= 0),
    CONSTRAINT channel_accounts_updated_after_created
        CHECK (updated_at_ms >= created_at_ms)
);

COMMENT ON TABLE channel_accounts IS
    'Chat provider accounts served by the connector host; routing identity and operational configuration, never secret material.';
COMMENT ON COLUMN channel_accounts.account_id IS
    'Authored channel account id, unique per universe; chat triggers point at it.';
COMMENT ON COLUMN channel_accounts.provider IS
    'Chat provider name copied from the document for indexed listing; open vocabulary, format-checked only.';
COMMENT ON COLUMN channel_accounts.provider_account_id IS
    'Provider-native account identity (Telegram bot username or id, WhatsApp phone number); unique per provider across the whole deployment.';
COMMENT ON COLUMN channel_accounts.revision IS
    'Document revision, bumped by every put.';
COMMENT ON COLUMN channel_accounts.document_json IS
    'Serialized ChannelAccountDocument; the credential is a grant reference, never a token.';
COMMENT ON COLUMN channel_accounts.created_at_ms IS
    'Creation time in Unix milliseconds.';
COMMENT ON COLUMN channel_accounts.updated_at_ms IS
    'Last document write in Unix milliseconds.';

-- ═══════════════════════════════════════════════════════════════════════════
-- channel_pairings
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE channel_pairings (
    -- ── Identity: the conversation itself ──────────────────────────────────
    universe_id uuid NOT NULL,
    -- Channel account the conversation arrived through.
    account_id text NOT NULL,
    -- Provider chat identifier of the conversation.
    chat_id text NOT NULL,

    -- ── The route ──────────────────────────────────────────────────────────
    -- The chat trigger that owns the conversation; a re-pair moves the
    -- chat to another trigger.
    bot_id text NOT NULL,
    trigger_id text NOT NULL,
    -- How the chat got its route: claimed by an open trigger's first
    -- contact, or paired by code.
    paired_via text NOT NULL,
    paired_at_ms bigint NOT NULL,

    -- The inbound lookup is the primary key: is this chat paired on this
    -- account, and to whom?
    PRIMARY KEY (universe_id, account_id, chat_id),
    CONSTRAINT channel_pairings_trigger_fk FOREIGN KEY (universe_id, bot_id, trigger_id)
        REFERENCES bot_triggers (universe_id, bot_id, trigger_id) ON DELETE CASCADE,
    CONSTRAINT channel_pairings_account_fk FOREIGN KEY (universe_id, account_id)
        REFERENCES channel_accounts (universe_id, account_id) ON DELETE CASCADE,

    CONSTRAINT channel_pairings_chat_id_not_empty
        CHECK (chat_id <> ''),
    CONSTRAINT channel_pairings_paired_via_known
        CHECK (paired_via IN ('open', 'code')),
    CONSTRAINT channel_pairings_paired_nonnegative
        CHECK (paired_at_ms >= 0)
);

-- The trigger's pairing list.
CREATE INDEX channel_pairings_trigger_idx
    ON channel_pairings (universe_id, bot_id, trigger_id);

COMMENT ON TABLE channel_pairings IS
    'The chat routing authority: one row per bound conversation (claimed by an open trigger or by pairing code); the paired trigger owns the chat while the row exists. Cascades from the trigger and the account.';
COMMENT ON COLUMN channel_pairings.paired_via IS
    'How the chat got its route: open (claimed by an open trigger''s first contact) or code.';
COMMENT ON COLUMN channel_pairings.bot_id IS
    'Bot whose chat trigger serves the conversation.';
COMMENT ON COLUMN channel_pairings.trigger_id IS
    'The chat trigger the conversation paired with; a re-pair moves the chat to another trigger.';
COMMENT ON COLUMN channel_pairings.account_id IS
    'Channel account the conversation arrived through.';
COMMENT ON COLUMN channel_pairings.chat_id IS
    'Provider chat identifier of the conversation.';
COMMENT ON COLUMN channel_pairings.paired_at_ms IS
    'When the conversation paired, in Unix milliseconds.';
