//! PostgreSQL storage for chat provider accounts and conversation pairings.
//! Accounts are what a `chat` bot trigger points at; pairings record which
//! conversations that trigger admitted.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use ::channels::{
    ChannelAccountRecord, ChannelAccountStore, ChannelError, ChannelPairingFilter,
    ChannelPairingRecord, ChannelPairingStore, validate_account_document,
};
use api::{BotId, BotTriggerId, ChannelAccountDocument, ChannelAccountId, ChannelProvider};
use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{PgStore, PgStoreError};

const ACCOUNT_COLUMNS: &str = r#"
    account_id, revision, document_json, created_at_ms, updated_at_ms
"#;

const PAIRING_COLUMNS: &str = r#"
    account_id, chat_id, bot_id, trigger_id, paired_via, paired_at_ms
"#;

// ── Accounts ────────────────────────────────────────────────────────────────

#[async_trait]
impl ChannelAccountStore for PgStore {
    async fn create_channel_account(
        &self,
        account_id: ChannelAccountId,
        document: ChannelAccountDocument,
        now_ms: i64,
    ) -> Result<ChannelAccountRecord, ChannelError> {
        self.ensure_universe()
            .await
            .map_err(|error| channel_store_error("ensure universe", error))?;
        validate_account_document(&document)?;
        let query = format!(
            r#"
            INSERT INTO channel_accounts (
                universe_id, account_id, provider, provider_account_id, revision,
                document_json, created_at_ms, updated_at_ms
            )
            VALUES ($1, $2, $3, $4, 1, $5, $6, $6)
            ON CONFLICT (universe_id, account_id) DO NOTHING
            RETURNING {ACCOUNT_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(account_id.as_str())
            .bind(document.provider.as_str())
            .bind(document.provider_account_id.as_str())
            .bind(json_value("serialize channel account document", &document)?)
            .bind(now_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| map_account_write_error("create channel account", error))?;
        let Some(row) = row else {
            return Err(ChannelError::AccountAlreadyExists { account_id });
        };
        account_from_row(&row)
    }

    async fn put_channel_account(
        &self,
        account_id: ChannelAccountId,
        document: ChannelAccountDocument,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<ChannelAccountRecord, ChannelError> {
        self.ensure_universe()
            .await
            .map_err(|error| channel_store_error("ensure universe", error))?;
        validate_account_document(&document)?;
        // A concurrent writer between the read and the write loses exactly one
        // retry; the recheck still enforces `expected_revision` against fresh
        // state, so the retry never bypasses the caller's guard.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let current = match self.read_channel_account(&account_id).await {
                Ok(current) => Some(current),
                Err(ChannelError::AccountNotFound { .. }) => None,
                Err(error) => return Err(error),
            };
            let Some(current) = current else {
                match self
                    .create_channel_account(account_id.clone(), document.clone(), now_ms)
                    .await
                {
                    Ok(created) => return Ok(created),
                    Err(ChannelError::AccountAlreadyExists { .. }) if attempt < 2 => continue,
                    Err(error) => return Err(error),
                }
            };
            if let Some(expected) = expected_revision
                && current.revision != expected
            {
                return Err(ChannelError::AccountRevisionConflict {
                    account_id,
                    expected,
                    actual: current.revision,
                });
            }
            let guard_revision = current.revision;
            let next_revision = guard_revision.checked_add(1).ok_or_else(|| {
                ChannelError::store(format!("channel account {account_id} revision overflow"))
            })?;
            match self
                .cas_write_account(
                    &account_id,
                    &document,
                    next_revision,
                    now_ms,
                    guard_revision,
                )
                .await?
            {
                Some(written) => return Ok(written),
                None if attempt < 2 => continue,
                None => {
                    let actual = self.read_channel_account(&account_id).await?.revision;
                    return Err(ChannelError::AccountRevisionConflict {
                        account_id,
                        expected: guard_revision,
                        actual,
                    });
                }
            }
        }
    }

    async fn read_channel_account(
        &self,
        account_id: &ChannelAccountId,
    ) -> Result<ChannelAccountRecord, ChannelError> {
        let query = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM channel_accounts \
             WHERE universe_id = $1 AND account_id = $2"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(account_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| channel_sql_error("read channel account", error))?;
        let Some(row) = row else {
            return Err(ChannelError::AccountNotFound {
                account_id: account_id.clone(),
            });
        };
        account_from_row(&row)
    }

    async fn list_channel_accounts(
        &self,
        provider: Option<ChannelProvider>,
    ) -> Result<Vec<ChannelAccountRecord>, ChannelError> {
        let query = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM channel_accounts \
             WHERE universe_id = $1 AND ($2::text IS NULL OR provider = $2) \
             ORDER BY account_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(provider.as_ref().map(ChannelProvider::as_str))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| channel_sql_error("list channel accounts", error))?;
        rows.iter().map(account_from_row).collect()
    }

    async fn delete_channel_account(
        &self,
        account_id: &ChannelAccountId,
    ) -> Result<ChannelAccountRecord, ChannelError> {
        let query = format!(
            "DELETE FROM channel_accounts WHERE universe_id = $1 AND account_id = $2 \
             RETURNING {ACCOUNT_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(account_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| channel_sql_error("delete channel account", error))?;
        let Some(row) = row else {
            return Err(ChannelError::AccountNotFound {
                account_id: account_id.clone(),
            });
        };
        account_from_row(&row)
    }
}

impl PgStore {
    /// Write `document` at `revision` over the row currently at
    /// `guard_revision`. Returns `None` when the guard no longer matches (a
    /// concurrent writer won).
    async fn cas_write_account(
        &self,
        account_id: &ChannelAccountId,
        document: &ChannelAccountDocument,
        revision: u64,
        now_ms: i64,
        guard_revision: u64,
    ) -> Result<Option<ChannelAccountRecord>, ChannelError> {
        let query = format!(
            r#"
            UPDATE channel_accounts SET
                provider = $3,
                provider_account_id = $4,
                revision = $5,
                document_json = $6,
                updated_at_ms = $7
            WHERE universe_id = $1 AND account_id = $2 AND revision = $8
            RETURNING {ACCOUNT_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(account_id.as_str())
            .bind(document.provider.as_str())
            .bind(document.provider_account_id.as_str())
            .bind(u64_to_i64(revision, "revision")?)
            .bind(json_value("serialize channel account document", document)?)
            .bind(now_ms)
            .bind(u64_to_i64(guard_revision, "revision")?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| map_account_write_error("write channel account", error))?;
        row.as_ref().map(account_from_row).transpose()
    }
}

// ── Pairings ────────────────────────────────────────────────────────────────

#[async_trait]
impl ChannelPairingStore for PgStore {
    async fn upsert_channel_pairing(
        &self,
        record: ChannelPairingRecord,
    ) -> Result<ChannelPairingRecord, ChannelError> {
        if record.chat_id.is_empty() {
            return Err(ChannelError::invalid("chatId must not be empty"));
        }
        let query = format!(
            r#"
            INSERT INTO channel_pairings (
                universe_id, account_id, chat_id, bot_id, trigger_id, paired_via, paired_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (universe_id, account_id, chat_id) DO UPDATE SET
                bot_id = EXCLUDED.bot_id,
                trigger_id = EXCLUDED.trigger_id,
                paired_via = EXCLUDED.paired_via,
                paired_at_ms = EXCLUDED.paired_at_ms
            RETURNING {PAIRING_COLUMNS}
            "#
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(record.account_id.as_str())
            .bind(record.chat_id.as_str())
            .bind(record.bot_id.as_str())
            .bind(record.trigger_id.as_str())
            .bind(record.paired_via.as_str())
            .bind(record.paired_at_ms)
            .fetch_one(&self.pool)
            .await
            .map_err(map_pairing_write_error)?;
        pairing_from_row(&row)
    }

    async fn read_channel_pairing(
        &self,
        account_id: &ChannelAccountId,
        chat_id: &str,
    ) -> Result<Option<ChannelPairingRecord>, ChannelError> {
        let query = format!(
            "SELECT {PAIRING_COLUMNS} FROM channel_pairings \
             WHERE universe_id = $1 AND account_id = $2 AND chat_id = $3"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(account_id.as_str())
            .bind(chat_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| channel_sql_error("read channel pairing", error))?;
        row.as_ref().map(pairing_from_row).transpose()
    }

    async fn list_channel_pairings(
        &self,
        filter: ChannelPairingFilter,
    ) -> Result<Vec<ChannelPairingRecord>, ChannelError> {
        let query = format!(
            "SELECT {PAIRING_COLUMNS} FROM channel_pairings \
             WHERE universe_id = $1 \
               AND ($2::text IS NULL OR account_id = $2) \
               AND ($3::text IS NULL OR bot_id = $3) \
               AND ($4::text IS NULL OR trigger_id = $4) \
               AND ($5::text IS NULL OR chat_id = $5) \
             ORDER BY paired_at_ms DESC, account_id, chat_id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(filter.account_id.as_ref().map(ChannelAccountId::as_str))
            .bind(filter.bot_id.as_ref().map(BotId::as_str))
            .bind(filter.trigger_id.as_ref().map(BotTriggerId::as_str))
            .bind(filter.chat_id.as_deref())
            .fetch_all(&self.pool)
            .await
            .map_err(|error| channel_sql_error("list channel pairings", error))?;
        rows.iter().map(pairing_from_row).collect()
    }

    async fn delete_channel_pairing(
        &self,
        account_id: &ChannelAccountId,
        chat_id: &str,
    ) -> Result<ChannelPairingRecord, ChannelError> {
        let query = format!(
            "DELETE FROM channel_pairings \
             WHERE universe_id = $1 AND account_id = $2 AND chat_id = $3 \
             RETURNING {PAIRING_COLUMNS}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(self.config.universe_id)
            .bind(account_id.as_str())
            .bind(chat_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| channel_sql_error("delete channel pairing", error))?;
        let Some(row) = row else {
            return Err(ChannelError::PairingNotFound {
                account_id: account_id.to_string(),
                chat_id: chat_id.to_owned(),
            });
        };
        pairing_from_row(&row)
    }
}

// ── Deployment-wide ─────────────────────────────────────────────────────────

/// Every channel account across universes, ordered by universe then account
/// id: the connector host's roster of accounts to serve. Disabled accounts
/// are skipped unless `include_disabled`.
pub async fn list_channel_accounts_all(
    pool: &PgPool,
    provider: Option<ChannelProvider>,
    include_disabled: bool,
) -> Result<Vec<(Uuid, ChannelAccountRecord)>, PgStoreError> {
    let query = format!(
        "SELECT universe_id, {ACCOUNT_COLUMNS} FROM channel_accounts \
         WHERE ($1::text IS NULL OR provider = $1) \
           AND ($2 OR COALESCE((document_json->>'enabled')::boolean, true)) \
         ORDER BY universe_id, account_id"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(provider.as_ref().map(ChannelProvider::as_str))
        .bind(include_disabled)
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|row| {
            let universe_id: Uuid = row.try_get("universe_id")?;
            let record = account_from_row(row).map_err(|error| PgStoreError::Store {
                message: error.to_string(),
            })?;
            Ok((universe_id, record))
        })
        .collect()
}

// ── Row decoding ────────────────────────────────────────────────────────────

fn account_from_row(row: &sqlx::postgres::PgRow) -> Result<ChannelAccountRecord, ChannelError> {
    let account_id: String = column(row, "account_id")?;
    let revision: i64 = column(row, "revision")?;
    Ok(ChannelAccountRecord {
        account_id: ChannelAccountId::try_new(account_id)
            .map_err(|error| store_message(format!("decode channel account id: {error}")))?,
        revision: i64_to_u64(revision, "revision")?,
        document: json_column(row, "document_json")?,
        created_at_ms: column(row, "created_at_ms")?,
        updated_at_ms: column(row, "updated_at_ms")?,
    })
}

fn pairing_from_row(row: &sqlx::postgres::PgRow) -> Result<ChannelPairingRecord, ChannelError> {
    let bot_id: String = column(row, "bot_id")?;
    let trigger_id: String = column(row, "trigger_id")?;
    let account_id: String = column(row, "account_id")?;
    let paired_via: String = column(row, "paired_via")?;
    let paired_via = match paired_via.as_str() {
        "open" => api::ChannelPairedVia::Open,
        "code" => api::ChannelPairedVia::Code,
        other => return Err(store_message(format!("unknown paired_via {other:?}"))),
    };
    Ok(ChannelPairingRecord {
        account_id: ChannelAccountId::try_new(account_id)
            .map_err(|error| store_message(format!("decode channel account id: {error}")))?,
        chat_id: column(row, "chat_id")?,
        bot_id: BotId::try_new(bot_id)
            .map_err(|error| store_message(format!("decode bot id: {error}")))?,
        trigger_id: BotTriggerId::try_new(trigger_id)
            .map_err(|error| store_message(format!("decode trigger id: {error}")))?,
        paired_via,
        paired_at_ms: column(row, "paired_at_ms")?,
    })
}

fn column<'r, T>(row: &'r sqlx::postgres::PgRow, name: &str) -> Result<T, ChannelError>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(name)
        .map_err(|error| channel_sql_error(&format!("decode column {name}"), error))
}

fn json_value<T: serde::Serialize>(
    action: &str,
    value: &T,
) -> Result<serde_json::Value, ChannelError> {
    serde_json::to_value(value).map_err(|error| store_message(format!("{action}: {error}")))
}

fn json_column<T: serde::de::DeserializeOwned>(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<T, ChannelError> {
    let value: serde_json::Value = column(row, name)?;
    serde_json::from_value(value).map_err(|error| store_message(format!("decode {name}: {error}")))
}

fn u64_to_i64(value: u64, name: &str) -> Result<i64, ChannelError> {
    i64::try_from(value).map_err(|_| ChannelError::invalid(format!("{name} exceeds i64::MAX")))
}

fn i64_to_u64(value: i64, name: &str) -> Result<u64, ChannelError> {
    u64::try_from(value).map_err(|_| store_message(format!("{name} is negative")))
}

fn constraint_name(error: &sqlx::Error) -> Option<&str> {
    error.as_database_error().and_then(|db| db.constraint())
}

fn map_account_write_error(action: &str, error: sqlx::Error) -> ChannelError {
    match constraint_name(&error) {
        Some("channel_accounts_provider_account_unique") => ChannelError::invalid(
            "this provider account is already registered as a channel account (an account serves exactly one universe)",
        ),
        _ => channel_sql_error(action, error),
    }
}

fn map_pairing_write_error(error: sqlx::Error) -> ChannelError {
    match constraint_name(&error) {
        Some("channel_pairings_trigger_fk") => {
            ChannelError::invalid("pairing references a chat trigger that does not exist")
        }
        Some("channel_pairings_account_fk") => {
            ChannelError::invalid("pairing references a channel account that does not exist")
        }
        _ => channel_sql_error("upsert channel pairing", error),
    }
}

fn channel_store_error(action: &str, error: PgStoreError) -> ChannelError {
    store_message(format!("{action}: {error}"))
}

fn channel_sql_error(action: &str, error: sqlx::Error) -> ChannelError {
    store_message(format!("{action}: {error}"))
}

fn store_message(message: impl Into<String>) -> ChannelError {
    ChannelError::Store {
        message: message.into(),
    }
}
