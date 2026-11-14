//! Deployment-scoped API key store.
//!
//! The store uses the deployment pool because authentication precedes
//! universe resolution. A key's authority is its row: scope, groups and the
//! actor flag. A key is addressed by its scope and key id; replacing its
//! secret changes only the hash and display prefix.

// Formatted SQL uses internal schema fragments; request values use bind parameters.
use std::collections::BTreeSet;

use api::{AccessScope, MethodGroup};
use auth::{ApiKeyError, ApiKeyRecord};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

const KEY_COLUMNS: &str = "key_id, key_prefix, universe_id, groups, assert_actor, display_name, created_by, created_at_ms, revoked_at_ms, last_used_at_ms";

/// What declaring a key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutApiKey {
    pub record: ApiKeyRecord,
    /// The key did not exist under this id or this secret.
    pub created: bool,
    /// An existing key now answers to the supplied secret instead of the one
    /// it had.
    pub secret_replaced: bool,
}

#[derive(Clone)]
pub struct PgApiKeyStore {
    pool: PgPool,
}

impl PgApiKeyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The universe a deployment key's request names by handle. Part of this
    /// store because the lookup happens while the request is authenticated,
    /// before any universe is resolved.
    pub async fn resolve_universe_handle(
        &self,
        handle: &str,
    ) -> Result<Option<Uuid>, crate::PgStoreError> {
        crate::deployment::resolve_universe_reference(&self.pool, handle).await
    }

    /// Persist a newly made key. A display prefix another key holds is
    /// `AlreadyExists`, so a generated secret is generated again; a taken key
    /// id is `KeyIdInUse`.
    pub async fn create_api_key(
        &self,
        key_hash: &str,
        record: &ApiKeyRecord,
    ) -> Result<(), ApiKeyError> {
        insert_key(&self.pool, key_hash, record).await
    }

    /// Declare a key with a supplied secret, so that afterwards the key of
    /// this id in this scope exists, answers to exactly this secret, and
    /// holds these groups and this name.
    ///
    /// A key that is missing is created. One that holds another secret has it
    /// replaced at once, with no period in which both work. What a key
    /// reaches and whether it asserts actors never change, and a revoked key
    /// is never restored: both are conflicts.
    ///
    /// A secret identifies one key. When it already belongs to a key of this
    /// scope that was never named, that key takes the declared id instead of
    /// a second key being made, which is how a key minted earlier comes under
    /// declaration.
    pub async fn put_api_key(&self, key: &auth::MintedApiKey) -> Result<PutApiKey, ApiKeyError> {
        let declared = &key.record;
        let groups: Vec<&str> = declared.groups.iter().map(|group| group.as_str()).collect();
        let mut transaction = self.pool.begin().await.map_err(map_sqlx_error)?;
        let named = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT key_hash, {KEY_COLUMNS} FROM api_keys
             WHERE universe_id IS NOT DISTINCT FROM $1 AND key_id = $2 FOR UPDATE"
        )))
        .bind(declared.scope.universe_id())
        .bind(&declared.key_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sqlx_error)?;
        // The key this declaration is about: the one of this id, or else the
        // unnamed one the secret already belongs to.
        let (existing, adopted) = match named {
            Some(row) => (Some(row), false),
            None => {
                let held = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "SELECT key_hash, {KEY_COLUMNS} FROM api_keys WHERE key_hash = $1 FOR UPDATE"
                )))
                .bind(&key.key_hash)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(map_sqlx_error)?;
                match held {
                    None => (None, false),
                    Some(row) => {
                        let holder = record_from_row(&row)?;
                        if holder.scope != declared.scope
                            || !auth::is_allocated_api_key_id(&holder.key_id)
                        {
                            return Err(secret_in_use());
                        }
                        (Some(row), true)
                    }
                }
            }
        };
        let Some(row) = existing else {
            insert_key(&mut *transaction, &key.key_hash, declared)
                .await
                .map_err(|error| match error {
                    // A supplied secret is not generated again.
                    ApiKeyError::AlreadyExists { .. } => secret_in_use(),
                    other => other,
                })?;
            transaction.commit().await.map_err(map_sqlx_error)?;
            return Ok(PutApiKey {
                record: declared.clone(),
                created: true,
                secret_replaced: false,
            });
        };
        let current = record_from_row(&row)?;
        let current_hash: String = row.try_get("key_hash").map_err(map_sqlx_error)?;
        if current.revoked_at_ms.is_some() {
            return Err(ApiKeyError::Conflict {
                message: format!(
                    "api key {} was revoked; declare the key under another key id with a new secret",
                    current.key_id
                ),
            });
        }
        if current.assert_actor != declared.assert_actor {
            return Err(ApiKeyError::Conflict {
                message: format!(
                    "api key {} already exists and whether a key may assert actors cannot change; declare a new key",
                    current.key_id
                ),
            });
        }
        let secret_replaced = current_hash != key.key_hash;
        let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE api_keys
             SET key_id = $2, key_hash = $3, key_prefix = $4, groups = $5, display_name = $6,
                 last_used_at_ms = CASE WHEN key_hash = $3 THEN last_used_at_ms END
             WHERE key_hash = $1 RETURNING {KEY_COLUMNS}"
        )))
        .bind(&current_hash)
        .bind(&declared.key_id)
        .bind(&key.key_hash)
        .bind(&declared.key_prefix)
        .bind(groups)
        .bind(declared.display_name.as_deref())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| match unique_violation(&error) {
            // The secret, or its display prefix, is another key's.
            Some(_) => secret_in_use(),
            None => map_sqlx_error(error),
        })?;
        transaction.commit().await.map_err(map_sqlx_error)?;
        debug_assert!(!(adopted && secret_replaced));
        Ok(PutApiKey {
            record: record_from_row(&updated)?,
            created: false,
            secret_replaced,
        })
    }

    /// Check, without writing, that the key this secret belongs to is active
    /// and is the one described: same id, scope, groups and actor flag.
    pub async fn verify_api_key(
        &self,
        key: &auth::MintedApiKey,
    ) -> Result<ApiKeyRecord, ApiKeyError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {KEY_COLUMNS} FROM api_keys WHERE key_hash = $1"
        )))
        .bind(&key.key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        let record = row
            .as_ref()
            .map(record_from_row)
            .transpose()?
            .ok_or_else(|| ApiKeyError::Invalid {
                message: "the supplied key is absent from this store; provision it first".into(),
            })?;
        if record.revoked_at_ms.is_some() {
            return Err(ApiKeyError::Invalid {
                message: "the supplied key was revoked; provision a different key".into(),
            });
        }
        if record.key_id != key.record.key_id
            || record.scope != key.record.scope
            || record.groups != key.record.groups
            || record.assert_actor != key.record.assert_actor
        {
            return Err(ApiKeyError::Invalid {
                message: "the supplied key exists with a different id or authority".into(),
            });
        }
        Ok(record)
    }

    /// Resolve an unrevoked key by secret hash; `None` for unknown and
    /// revoked keys alike.
    pub async fn resolve_api_key(
        &self,
        key_hash: &str,
        observed_at_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        let Some(row) = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {KEY_COLUMNS} FROM api_keys WHERE key_hash = $1 AND revoked_at_ms IS NULL"
        )))
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        else {
            return Ok(None);
        };
        let record = record_from_row(&row)?;
        // Usage is coarse on purpose: authentication stays a read.
        if record.last_used_at_ms.is_none_or(|last| {
            last.saturating_add(auth::API_KEY_LAST_USED_RESOLUTION_MS) <= observed_at_ms
        }) {
            sqlx::query(
                "UPDATE api_keys SET last_used_at_ms = $2 WHERE key_hash = $1 AND \
                 (last_used_at_ms IS NULL OR last_used_at_ms < $2)",
            )
            .bind(key_hash)
            .bind(ms_to_i64(observed_at_ms)?)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        }
        Ok(Some(record))
    }

    /// Every key, or the keys of one scope.
    pub async fn list_api_keys(
        &self,
        scope: Option<AccessScope>,
    ) -> Result<Vec<ApiKeyRecord>, ApiKeyError> {
        let (filtered, universe_id) = match scope {
            None => (false, None),
            Some(scope) => (true, scope.universe_id()),
        };
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {KEY_COLUMNS} FROM api_keys
             WHERE NOT $1 OR universe_id IS NOT DISTINCT FROM $2
             ORDER BY created_at_ms, key_id"
        )))
        .bind(filtered)
        .bind(universe_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        .iter()
        .map(record_from_row)
        .collect()
    }

    /// Replace an active key's secret with a generated one, atomically: the
    /// old secret stops working in the same statement. `None` for an unknown
    /// or revoked key.
    pub async fn rotate_api_key(
        &self,
        scope: AccessScope,
        key_id: &str,
    ) -> Result<Option<auth::MintedApiKey>, ApiKeyError> {
        for _ in 0..3 {
            let secret = auth::generate_prefixed_secret(auth::API_KEY_SECRET_PREFIX);
            let key_hash = auth::api_key_hash(&secret);
            let result = sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE api_keys SET key_hash = $3, key_prefix = $4, last_used_at_ms = NULL
                 WHERE universe_id IS NOT DISTINCT FROM $1 AND key_id = $2
                   AND revoked_at_ms IS NULL
                 RETURNING {KEY_COLUMNS}"
            )))
            .bind(scope.universe_id())
            .bind(key_id)
            .bind(&key_hash)
            .bind(auth::api_key_display_prefix(&secret))
            .fetch_optional(&self.pool)
            .await;
            match result {
                Ok(Some(row)) => {
                    return Ok(Some(auth::MintedApiKey {
                        secret: auth::SecretValue::new(secret),
                        key_hash,
                        record: record_from_row(&row)?,
                    }));
                }
                Ok(None) => return Ok(None),
                // A display-prefix collision: generate again.
                Err(error) if unique_violation(&error).is_some() => continue,
                Err(error) => return Err(map_sqlx_error(error)),
            }
        }
        Err(ApiKeyError::Store {
            message: "could not allocate a unique api key prefix".into(),
        })
    }

    /// Revoke a key. `None` for an unknown key; a revoked key stays revoked
    /// at its first revocation time.
    pub async fn revoke_api_key(
        &self,
        scope: AccessScope,
        key_id: &str,
        now_ms: u64,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE api_keys SET revoked_at_ms = COALESCE(revoked_at_ms, $3)
             WHERE universe_id IS NOT DISTINCT FROM $1 AND key_id = $2 RETURNING {KEY_COLUMNS}"
        )))
        .bind(scope.universe_id())
        .bind(key_id)
        .bind(ms_to_i64(now_ms)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?
        .as_ref()
        .map(record_from_row)
        .transpose()
    }
}

async fn insert_key<'c, E>(
    executor: E,
    key_hash: &str,
    record: &ApiKeyRecord,
) -> Result<(), ApiKeyError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let groups: Vec<&str> = record.groups.iter().map(|group| group.as_str()).collect();
    sqlx::query(
        "INSERT INTO api_keys (key_hash, key_id, key_prefix, universe_id, groups, assert_actor, display_name, created_by, created_at_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(key_hash)
    .bind(&record.key_id)
    .bind(&record.key_prefix)
    .bind(record.scope.universe_id())
    .bind(groups)
    .bind(record.assert_actor)
    .bind(record.display_name.as_deref())
    .bind(json!(record.created_by))
    .bind(ms_to_i64(record.created_at_ms)?)
    .execute(executor)
    .await
    .map(|_| ())
    .map_err(|error| match unique_violation(&error) {
        Some(KEY_ID_INDEX) => ApiKeyError::KeyIdInUse {
            key_id: record.key_id.clone(),
        },
        Some(_) => ApiKeyError::AlreadyExists {
            key_prefix: record.key_prefix.clone(),
        },
        None => map_sqlx_error(error),
    })
}

const KEY_ID_INDEX: &str = "api_keys_scope_key_id_idx";

/// The constraint a unique violation broke, if that is what `error` is.
fn unique_violation(error: &sqlx::Error) -> Option<&str> {
    match error {
        sqlx::Error::Database(error) if error.is_unique_violation() => {
            Some(error.constraint().unwrap_or_default())
        }
        _ => None,
    }
}

fn secret_in_use() -> ApiKeyError {
    ApiKeyError::Conflict {
        message:
            "the supplied secret already belongs to another api key; supply a secret of its own"
                .into(),
    }
}

fn record_from_row(row: &PgRow) -> Result<ApiKeyRecord, ApiKeyError> {
    let universe_id: Option<Uuid> = row.try_get("universe_id").map_err(map_sqlx_error)?;
    let groups = row
        .try_get::<Vec<String>, _>("groups")
        .map_err(map_sqlx_error)?
        .iter()
        .map(|name| {
            MethodGroup::parse(name).ok_or_else(|| ApiKeyError::Store {
                message: format!("unknown method group in api_keys row: {name}"),
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(ApiKeyRecord {
        key_id: row.try_get("key_id").map_err(map_sqlx_error)?,
        key_prefix: row.try_get("key_prefix").map_err(map_sqlx_error)?,
        scope: universe_id.map_or(AccessScope::Deployment, |universe_id| {
            AccessScope::Universe { universe_id }
        }),
        groups,
        assert_actor: row.try_get("assert_actor").map_err(map_sqlx_error)?,
        created_by: serde_json::from_value(row.try_get("created_by").map_err(map_sqlx_error)?)
            .map_err(|error| ApiKeyError::Store {
                message: format!("decode api key created_by: {error}"),
            })?,
        display_name: row.try_get("display_name").map_err(map_sqlx_error)?,
        created_at_ms: i64_to_ms(row.try_get("created_at_ms").map_err(map_sqlx_error)?)?,
        revoked_at_ms: row
            .try_get::<Option<i64>, _>("revoked_at_ms")
            .map_err(map_sqlx_error)?
            .map(i64_to_ms)
            .transpose()?,
        last_used_at_ms: row
            .try_get::<Option<i64>, _>("last_used_at_ms")
            .map_err(map_sqlx_error)?
            .map(i64_to_ms)
            .transpose()?,
    })
}

fn ms_to_i64(value: u64) -> Result<i64, ApiKeyError> {
    i64::try_from(value).map_err(|_| ApiKeyError::Store {
        message: format!("timestamp out of range: {value}"),
    })
}

fn i64_to_ms(value: i64) -> Result<u64, ApiKeyError> {
    u64::try_from(value).map_err(|_| ApiKeyError::Store {
        message: format!("negative timestamp in api_keys row: {value}"),
    })
}

fn map_sqlx_error(error: sqlx::Error) -> ApiKeyError {
    ApiKeyError::Store {
        message: error.to_string(),
    }
}
