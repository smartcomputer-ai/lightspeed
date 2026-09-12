//! Embedded, immutable PostgreSQL schema migrations.
//!
//! Production processes verify this ledger at startup. Only the explicit
//! `lightspeed-server migrate` command applies migrations.

use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};
use sqlx::{Acquire as _, Executor as _, PgPool, Postgres, Transaction};

use crate::PgStoreError;

/// Serializes every Lightspeed schema inspection and migration in a database.
const MIGRATION_ADVISORY_LOCK_ID: i64 = 0x4c53_5047_4d49_4752;

/// Relations owned by the embedded migrations. Their presence without a
/// migration ledger is evidence of a pre-ledger Lightspeed database, not an
/// empty schema that can safely receive the initial migration.
const LIGHTSPEED_TABLES: &[&str] = &[
    "agent_profiles",
    "api_keys",
    "auth_clients",
    "auth_flows",
    "auth_grants",
    "auth_providers",
    "auth_secrets",
    "bot_events",
    "bot_triggers",
    "bots",
    "cas_blob_edges",
    "cas_blobs",
    "cas_bot_event_roots",
    "cas_session_roots",
    "channel_accounts",
    "channel_pairings",
    "environment_credentials",
    "environment_incarnations",
    "environment_provider_bindings",
    "environment_providers",
    "environment_registration_keys",
    "environments",
    "mcp_servers",
    "session_checkpoints",
    "session_events",
    "sessions",
    "universes",
    "vfs_snapshots",
    "vfs_workspaces",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedMigration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[EmbeddedMigration] = &[
    EmbeddedMigration {
        version: 1,
        name: "core",
        sql: include_str!("../migrations/001_core.sql"),
    },
    EmbeddedMigration {
        version: 2,
        name: "vfs",
        sql: include_str!("../migrations/002_vfs.sql"),
    },
    EmbeddedMigration {
        version: 3,
        name: "mcp",
        sql: include_str!("../migrations/003_mcp.sql"),
    },
    EmbeddedMigration {
        version: 4,
        name: "auth",
        sql: include_str!("../migrations/004_auth.sql"),
    },
    EmbeddedMigration {
        version: 5,
        name: "environments",
        sql: include_str!("../migrations/005_environments.sql"),
    },
    EmbeddedMigration {
        version: 6,
        name: "agent_profiles",
        sql: include_str!("../migrations/006_agent_profiles.sql"),
    },
    EmbeddedMigration {
        version: 7,
        name: "api_keys",
        sql: include_str!("../migrations/007_api_keys.sql"),
    },
    EmbeddedMigration {
        version: 8,
        name: "bots",
        sql: include_str!("../migrations/008_bots.sql"),
    },
    EmbeddedMigration {
        version: 9,
        name: "channels",
        sql: include_str!("../migrations/009_channels.sql"),
    },
    EmbeddedMigration {
        version: 10,
        name: "independent_environment_lifecycle",
        sql: include_str!("../migrations/010_independent_environment_lifecycle.sql"),
    },
];

pub const REQUIRED_SCHEMA_REVISION: i64 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaStatus {
    pub current_revision: i64,
    pub required_revision: i64,
    pub pending: Vec<i64>,
}

impl SchemaStatus {
    pub fn is_current(&self) -> bool {
        self.pending.is_empty() && self.current_revision == self.required_revision
    }
}

pub async fn migrate(pool: &PgPool) -> Result<SchemaStatus, PgStoreError> {
    with_migration_lock(pool, |connection| {
        Box::pin(async move {
            if !ledger_exists(connection).await? {
                reject_unledgered_schema(connection).await?;
                ensure_ledger(connection).await?;
            }
            let applied = read_ledger(connection).await?;
            if applied.is_empty() {
                reject_unledgered_schema(connection).await?;
            }
            validate_applied(&applied)?;

            for migration in MIGRATIONS {
                if applied.contains_key(&migration.version) {
                    continue;
                }
                apply_one(connection, migration).await?;
            }

            status_with_ledger(connection).await
        })
    })
    .await
}

pub async fn schema_status(pool: &PgPool) -> Result<SchemaStatus, PgStoreError> {
    with_migration_lock(pool, |connection| {
        Box::pin(async move {
            if !ledger_exists(connection).await? {
                reject_unledgered_schema(connection).await?;
                return Ok(SchemaStatus {
                    current_revision: 0,
                    required_revision: REQUIRED_SCHEMA_REVISION,
                    pending: MIGRATIONS
                        .iter()
                        .map(|migration| migration.version)
                        .collect(),
                });
            }
            if read_ledger(connection).await?.is_empty() {
                reject_unledgered_schema(connection).await?;
            }
            status_with_ledger(connection).await
        })
    })
    .await
}

pub async fn verify_schema(pool: &PgPool) -> Result<SchemaStatus, PgStoreError> {
    let status = schema_status(pool).await?;
    if status.is_current() {
        Ok(status)
    } else {
        Err(PgStoreError::MigrationRequired {
            current_revision: status.current_revision,
            required_revision: status.required_revision,
        })
    }
}

type LockedFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<SchemaStatus, PgStoreError>> + Send + 'a>,
>;

async fn with_migration_lock<F>(pool: &PgPool, operation: F) -> Result<SchemaStatus, PgStoreError>
where
    F: for<'a> FnOnce(&'a mut sqlx::PgConnection) -> LockedFuture<'a>,
{
    let mut connection = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_ADVISORY_LOCK_ID)
        .execute(&mut *connection)
        .await?;

    let result = operation(&mut connection).await;
    let unlock_result = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_ADVISORY_LOCK_ID)
        .execute(&mut *connection)
        .await;
    match (result, unlock_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
        (Ok(status), Ok(_)) => Ok(status),
    }
}

async fn ledger_exists(connection: &mut sqlx::PgConnection) -> Result<bool, PgStoreError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT to_regclass(current_schema() || '.schema_migrations') IS NOT NULL",
    )
    .fetch_one(connection)
    .await?;
    Ok(exists)
}

async fn reject_unledgered_schema(connection: &mut sqlx::PgConnection) -> Result<(), PgStoreError> {
    let names: Vec<String> = LIGHTSPEED_TABLES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let relations: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT relation.relname
        FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
        WHERE namespace.nspname = current_schema()
          AND relation.relkind IN ('r', 'p')
          AND relation.relname = ANY($1::text[])
        ORDER BY relation.relname
        "#,
    )
    .bind(names)
    .fetch_all(connection)
    .await?;
    if relations.is_empty() {
        Ok(())
    } else {
        Err(PgStoreError::UnledgeredSchema { relations })
    }
}

async fn ensure_ledger(connection: &mut sqlx::PgConnection) -> Result<(), PgStoreError> {
    connection
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version bigint PRIMARY KEY,
                name text NOT NULL,
                checksum text NOT NULL,
                applied_at timestamptz NOT NULL DEFAULT now(),
                CONSTRAINT schema_migrations_version_positive CHECK (version > 0),
                CONSTRAINT schema_migrations_name_present CHECK (name <> ''),
                CONSTRAINT schema_migrations_checksum_sha256
                    CHECK (checksum ~ '^[0-9a-f]{64}$')
            )
            "#,
        )
        .await?;
    Ok(())
}

async fn read_ledger(
    connection: &mut sqlx::PgConnection,
) -> Result<BTreeMap<i64, (String, String)>, PgStoreError> {
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT version, name, checksum FROM schema_migrations ORDER BY version")
            .fetch_all(connection)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(version, name, checksum)| (version, (name, checksum)))
        .collect())
}

fn validate_applied(applied: &BTreeMap<i64, (String, String)>) -> Result<(), PgStoreError> {
    if let Some(highest) = applied.keys().next_back().copied() {
        for version in 1..=highest {
            if !applied.contains_key(&version) {
                return Err(PgStoreError::MigrationLedgerGap {
                    missing_version: version,
                });
            }
        }
    }
    for (&version, (actual_name, actual_checksum)) in applied {
        let Some(expected) = MIGRATIONS
            .iter()
            .find(|migration| migration.version == version)
        else {
            return Err(PgStoreError::SchemaTooNew {
                current_revision: version,
                required_revision: REQUIRED_SCHEMA_REVISION,
            });
        };
        if actual_name != expected.name {
            return Err(PgStoreError::MigrationNameChanged {
                version,
                expected: expected.name,
                actual: actual_name.clone(),
            });
        }
        let expected_checksum = checksum(expected.sql);
        if actual_checksum != &expected_checksum {
            return Err(PgStoreError::MigrationChecksumChanged {
                version,
                name: expected.name,
                expected: expected_checksum,
                actual: actual_checksum.clone(),
            });
        }
    }
    Ok(())
}

async fn status_with_ledger(
    connection: &mut sqlx::PgConnection,
) -> Result<SchemaStatus, PgStoreError> {
    let applied = read_ledger(connection).await?;
    validate_applied(&applied)?;
    let current_revision = applied.keys().next_back().copied().unwrap_or(0);
    let pending = MIGRATIONS
        .iter()
        .filter(|migration| !applied.contains_key(&migration.version))
        .map(|migration| migration.version)
        .collect();
    Ok(SchemaStatus {
        current_revision,
        required_revision: REQUIRED_SCHEMA_REVISION,
        pending,
    })
}

async fn apply_one(
    connection: &mut sqlx::PgConnection,
    migration: &EmbeddedMigration,
) -> Result<(), PgStoreError> {
    let mut transaction: Transaction<'_, Postgres> = connection.begin().await?;
    transaction.execute(migration.sql).await?;
    sqlx::query("INSERT INTO schema_migrations (version, name, checksum) VALUES ($1, $2, $3)")
        .bind(migration.version)
        .bind(migration.name)
        .bind(checksum(migration.sql))
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

fn checksum(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn embedded_migrations_are_contiguous_and_revision_matches() {
        let versions: Vec<_> = MIGRATIONS
            .iter()
            .map(|migration| migration.version)
            .collect();
        assert_eq!(versions, (1..=REQUIRED_SCHEMA_REVISION).collect::<Vec<_>>());
        assert!(MIGRATIONS.iter().all(|migration| !migration.sql.is_empty()));
        assert!(
            MIGRATIONS
                .iter()
                .all(|migration| checksum(migration.sql).len() == 64)
        );
        assert!(LIGHTSPEED_TABLES.windows(2).all(|pair| pair[0] < pair[1]));
        // Relations the ledger owns: created by some migration and not
        // dropped by a later one.
        let dropped_tables: BTreeSet<_> = MIGRATIONS
            .iter()
            .flat_map(|migration| migration.sql.lines())
            .filter_map(|line| line.trim().strip_prefix("DROP TABLE IF EXISTS "))
            .map(|remainder| remainder.trim_end_matches(';').to_owned())
            .collect();
        let migrated_tables: BTreeSet<_> = MIGRATIONS
            .iter()
            .flat_map(|migration| migration.sql.lines())
            .filter_map(|line| line.trim().strip_prefix("CREATE TABLE IF NOT EXISTS "))
            .map(|remainder| remainder.trim_end_matches(" (").to_owned())
            .filter(|table| !dropped_tables.contains(table))
            .collect();
        assert_eq!(
            migrated_tables,
            LIGHTSPEED_TABLES
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        );
    }

    #[test]
    fn changed_checksum_is_rejected() {
        let migration = MIGRATIONS[0];
        let applied = BTreeMap::from([(
            migration.version,
            (migration.name.to_owned(), "0".repeat(64)),
        )]);
        assert!(matches!(
            validate_applied(&applied),
            Err(PgStoreError::MigrationChecksumChanged { version: 1, .. })
        ));
    }

    #[test]
    fn ledger_gap_is_rejected() {
        let migration = MIGRATIONS[1];
        let applied = BTreeMap::from([(
            migration.version,
            (migration.name.to_owned(), checksum(migration.sql)),
        )]);
        assert!(matches!(
            validate_applied(&applied),
            Err(PgStoreError::MigrationLedgerGap { missing_version: 1 })
        ));
    }
}
