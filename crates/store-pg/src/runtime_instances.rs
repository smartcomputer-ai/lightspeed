//! Gateway process leases and fenced registered-daemon ownership.
//!
//! These are infrastructure records, separate from public environment DTOs.
//! PostgreSQL supplies every lease and connection timestamp. An expired
//! instance cannot renew its identity or claim another control connection.

use std::time::Duration;

use environments::{DaemonBuild, EnvironmentId, REGISTERED_STALE_AFTER_MS};
use sqlx::{PgPool, Postgres, Row, pool::PoolConnection, postgres::PgRow};
use uuid::Uuid;

use crate::{PgStore, PgStoreError, changes::ChangeKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeInstance {
    pub instance_id: Uuid,
    pub roles: Vec<String>,
    pub peer_url: Option<String>,
    pub started_at_ms: i64,
    pub lease_expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredConnectionOwner {
    pub owner_instance_id: Uuid,
    pub control_connection_id: String,
    pub peer_url: Option<String>,
}

pub async fn register_runtime_instance(
    pool: &PgPool,
    instance_id: Uuid,
    roles: &[String],
    peer_url: Option<&str>,
    lease: Duration,
) -> Result<RuntimeInstance, PgStoreError> {
    let lease_ms = lease_millis(lease)?;
    let row = sqlx::query(
        r#"
        WITH clock AS (SELECT (extract(epoch FROM clock_timestamp()) * 1000)::bigint AS now_ms)
        INSERT INTO runtime_instances (instance_id, roles, peer_url, started_at_ms, lease_expires_at_ms)
        SELECT $1, $2, $3, now_ms, now_ms + $4 FROM clock
        RETURNING instance_id, roles, peer_url, started_at_ms, lease_expires_at_ms
        "#,
    )
    .bind(instance_id)
    .bind(roles)
    .bind(peer_url)
    .bind(lease_ms)
    .fetch_one(pool)
    .await?;
    Ok(instance_from_row(&row)?)
}

/// Only an unexpired lease can be renewed. Recovering an expired process
/// requires a new identity and new daemon admissions.
pub async fn renew_runtime_instance(
    pool: &PgPool,
    instance_id: Uuid,
    lease: Duration,
) -> Result<bool, PgStoreError> {
    let lease_ms = lease_millis(lease)?;
    let mut tx = pool.begin().await?;
    // Check expiry after acquiring the row lock, in a new statement. A
    // renewal waiting behind another transaction must not use the time from
    // before it waited to resurrect an expired instance.
    let found =
        sqlx::query("SELECT instance_id FROM runtime_instances WHERE instance_id = $1 FOR UPDATE")
            .bind(instance_id)
            .fetch_optional(&mut *tx)
            .await?;
    if found.is_none() {
        return Ok(false);
    }
    let changed = sqlx::query(
        r#"
        UPDATE runtime_instances SET
            lease_expires_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint + $2
        WHERE instance_id = $1
          AND lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint
        "#,
    )
    .bind(instance_id)
    .bind(lease_ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(changed.rows_affected() != 0)
}

pub async fn read_live_runtime_instance(
    pool: &PgPool,
    instance_id: Uuid,
) -> Result<Option<RuntimeInstance>, PgStoreError> {
    let row = sqlx::query(
        r#"
        SELECT instance_id, roles, peer_url, started_at_ms, lease_expires_at_ms
        FROM runtime_instances WHERE instance_id = $1
          AND lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint
        "#,
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(instance_from_row).transpose()?)
}

pub async fn delete_runtime_instance(pool: &PgPool, instance_id: Uuid) -> Result<(), PgStoreError> {
    sqlx::query("DELETE FROM runtime_instances WHERE instance_id = $1")
        .bind(instance_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn read_registered_connection_owner(
    pool: &PgPool,
    universe_id: Uuid,
    environment_id: &str,
) -> Result<Option<RegisteredConnectionOwner>, PgStoreError> {
    let row = sqlx::query(
        r#"
        SELECT e.owner_instance_id, e.control_connection_id, i.peer_url
        FROM environments e JOIN runtime_instances i ON i.instance_id = e.owner_instance_id
        WHERE e.universe_id = $1 AND e.environment_id = $2
          AND e.source_kind = 'registered' AND e.state = 'open' AND e.connected
          AND i.lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint
        "#,
    )
    .bind(universe_id)
    .bind(environment_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(owner_from_row).transpose()?)
}

/// Revalidate a replica's routes and controls in one database round trip.
/// Missing entries are no longer backed by a live owner.
pub async fn read_registered_connection_owners(
    pool: &PgPool,
    environments: &[(Uuid, String)],
) -> Result<Vec<(Uuid, String, RegisteredConnectionOwner)>, PgStoreError> {
    if environments.is_empty() {
        return Ok(Vec::new());
    }
    let universes: Vec<_> = environments.iter().map(|(universe, _)| *universe).collect();
    let ids: Vec<_> = environments.iter().map(|(_, id)| id.as_str()).collect();
    let rows = sqlx::query(
        r#"
        SELECT e.universe_id, e.environment_id, e.owner_instance_id, e.control_connection_id, i.peer_url
        FROM unnest($1::uuid[], $2::text[]) AS watched(universe_id, environment_id)
        JOIN environments e USING (universe_id, environment_id)
        JOIN runtime_instances i ON i.instance_id = e.owner_instance_id
        WHERE e.source_kind = 'registered' AND e.state = 'open' AND e.connected
          AND i.lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint
        "#,
    )
    .bind(universes)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("universe_id")?,
                row.try_get("environment_id")?,
                owner_from_row(row)?,
            ))
        })
        .collect()
}

impl PgStore {
    /// A successful admission is the only operation that may replace an
    /// owner. Heartbeats and disconnects must retain both ownership IDs.
    pub async fn claim_registered_connection(
        &self,
        environment_id: &EnvironmentId,
        instance_id: Uuid,
        control_connection_id: &str,
        daemon_build: &DaemonBuild,
        daemon_public_key: &str,
    ) -> Result<bool, PgStoreError> {
        let protocol_version =
            i32::try_from(daemon_build.protocol_version).map_err(|_| PgStoreError::Store {
                message: "daemon protocol version is out of range".to_owned(),
            })?;
        let result = sqlx::query(
            r#"
            UPDATE environments SET owner_instance_id = $3, control_connection_id = $4,
                connected = true,
                daemon_version = $5, daemon_git_sha = $6, daemon_protocol_version = $7,
                connection_changed_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
                last_seen_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
                updated_at_ms = GREATEST(updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
            WHERE universe_id = $1 AND environment_id = $2
              AND source_kind = 'registered' AND state = 'open' AND daemon_public_key = $8
              AND EXISTS (SELECT 1 FROM runtime_instances WHERE instance_id = $3
                  AND lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(environment_id.as_str())
        .bind(instance_id)
        .bind(control_connection_id)
        .bind(daemon_build.version.as_deref())
        .bind(daemon_build.git_sha.as_deref())
        .bind(protocol_version)
        .bind(daemon_public_key)
        .execute(&self.pool)
        .await?;
        Ok(self.connection_changed(environment_id, result.rows_affected()))
    }

    pub async fn read_registered_connection_owner(
        &self,
        environment_id: &EnvironmentId,
    ) -> Result<Option<RegisteredConnectionOwner>, PgStoreError> {
        read_registered_connection_owner(
            &self.pool,
            self.config.universe_id,
            environment_id.as_str(),
        )
        .await
    }

    /// A stale connection cannot stamp its successor or restore connectivity.
    pub async fn stamp_owned_registered_connection(
        &self,
        environment_id: &EnvironmentId,
        instance_id: Uuid,
        control_connection_id: &str,
    ) -> Result<bool, PgStoreError> {
        let result = sqlx::query(
            r#"
            UPDATE environments SET
                last_seen_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
                updated_at_ms = GREATEST(updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
            WHERE universe_id = $1 AND environment_id = $2
              AND source_kind = 'registered' AND state = 'open' AND connected
              AND owner_instance_id = $3 AND control_connection_id = $4
              AND EXISTS (SELECT 1 FROM runtime_instances WHERE instance_id = $3
                  AND lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
            "#,
        )
        .bind(self.config.universe_id)
        .bind(environment_id.as_str())
        .bind(instance_id)
        .bind(control_connection_id)
        .execute(&self.pool)
        .await?;
        // Liveness alone does not invalidate routes or change their ownership.
        Ok(result.rows_affected() != 0)
    }

    pub async fn disconnect_owned_registered_connection(
        &self,
        environment_id: &EnvironmentId,
        instance_id: Uuid,
        control_connection_id: &str,
    ) -> Result<bool, PgStoreError> {
        let result = sqlx::query(
            r#"
            UPDATE environments SET connected = false,
                owner_instance_id = NULL, control_connection_id = NULL,
                connection_changed_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
                updated_at_ms = GREATEST(updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
            WHERE universe_id = $1 AND environment_id = $2
              AND owner_instance_id = $3 AND control_connection_id = $4
            "#,
        )
        .bind(self.config.universe_id)
        .bind(environment_id.as_str())
        .bind(instance_id)
        .bind(control_connection_id)
        .execute(&self.pool)
        .await?;
        Ok(self.connection_changed(environment_id, result.rows_affected()))
    }

    /// A deliberate departure closes an ephemeral identity only while this
    /// exact control connection still owns it, in the same write.
    pub async fn depart_owned_registered_connection(
        &self,
        environment_id: &EnvironmentId,
        instance_id: Uuid,
        control_connection_id: &str,
    ) -> Result<bool, PgStoreError> {
        let result = sqlx::query(
            r#"
            UPDATE environments e SET connected = false,
                owner_instance_id = NULL, control_connection_id = NULL,
                state = 'closed', close_reason = 'departed', state_message = NULL,
                connection_changed_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
                state_changed_at_ms = GREATEST(e.updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint),
                closed_at_ms = GREATEST(e.updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint),
                updated_at_ms = GREATEST(e.updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
            FROM environment_registration_keys k
            WHERE e.universe_id = $1 AND e.environment_id = $2 AND e.state = 'open'
              AND e.owner_instance_id = $3 AND e.control_connection_id = $4
              AND k.universe_id = e.universe_id AND k.registration_key_id = e.registration_key_id
              AND k.identity_mode = 'ephemeral'
            "#,
        )
        .bind(self.config.universe_id)
        .bind(environment_id.as_str())
        .bind(instance_id)
        .bind(control_connection_id)
        .execute(&self.pool)
        .await?;
        Ok(self.connection_changed(environment_id, result.rows_affected()))
    }

    fn connection_changed(&self, environment_id: &EnvironmentId, rows: u64) -> bool {
        if rows != 0 {
            self.changed(ChangeKind::Environment, environment_id.as_str());
        }
        rows != 0
    }
}

/// Cleanup after a drain only affects rows that still name this instance.
pub async fn disconnect_runtime_instance_connections(
    pool: &PgPool,
    instance_id: Uuid,
) -> Result<u64, PgStoreError> {
    let result = sqlx::query(
        r#"
        UPDATE environments SET connected = false, owner_instance_id = NULL, control_connection_id = NULL,
            connection_changed_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
            updated_at_ms = GREATEST(updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
        WHERE owner_instance_id = $1
        "#,
    )
    .bind(instance_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Expiry repair replaces the old process-start repair. Missing ownership
/// also covers an admission interrupted between creating its row and claiming
/// its control connection, after the normal stale interval. Never modifies an
/// environment with a live owner.
pub async fn sweep_expired_registered_connections(pool: &PgPool) -> Result<u64, PgStoreError> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        r#"
        WITH expired AS MATERIALIZED (
            SELECT e.universe_id, e.environment_id, e.owner_instance_id, e.control_connection_id
            FROM environments e
            WHERE e.source_kind = 'registered' AND e.connected
              AND (e.owner_instance_id IS NOT NULL OR e.last_seen_at_ms <
                  (extract(epoch FROM clock_timestamp()) * 1000)::bigint - $1)
              AND NOT EXISTS (SELECT 1 FROM runtime_instances i WHERE i.instance_id = e.owner_instance_id
                  AND i.lease_expires_at_ms > (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
        )
        UPDATE environments e SET connected = false, owner_instance_id = NULL, control_connection_id = NULL,
            connection_changed_at_ms = (extract(epoch FROM clock_timestamp()) * 1000)::bigint,
            updated_at_ms = GREATEST(e.updated_at_ms, (extract(epoch FROM clock_timestamp()) * 1000)::bigint)
        FROM expired
        WHERE e.universe_id = expired.universe_id AND e.environment_id = expired.environment_id
          AND e.connected
          AND e.owner_instance_id IS NOT DISTINCT FROM expired.owner_instance_id
          AND e.control_connection_id IS NOT DISTINCT FROM expired.control_connection_id
        "#,
    )
    .bind(REGISTERED_STALE_AFTER_MS)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM runtime_instances i
        WHERE i.lease_expires_at_ms <= (extract(epoch FROM clock_timestamp()) * 1000)::bigint
          AND NOT EXISTS (SELECT 1 FROM environments e WHERE e.owner_instance_id = i.instance_id AND e.connected)
        "#,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeLoop {
    EnvironmentLifecycle,
    RegisteredSweep,
    EnvironmentDisplaySweep,
}

impl RuntimeLoop {
    fn lock_id(self) -> i64 {
        match self {
            Self::EnvironmentLifecycle => 0x4c53_454e_565f_4c43,
            Self::RegisteredSweep => 0x4c53_454e_565f_5253,
            Self::EnvironmentDisplaySweep => 0x4c53_454e_565f_4453,
        }
    }
}

/// Leadership is attached to one PostgreSQL session. Never return that
/// session to the pool while holding the advisory lock.
pub struct RuntimeLoopLeader {
    connection: PoolConnection<Postgres>,
}

struct AcquiringLoopLock(Option<PoolConnection<Postgres>>);

impl Drop for AcquiringLoopLock {
    fn drop(&mut self) {
        // The server may acquire the lock before its reply reaches the task.
        // An error or cancellation must close that possibly locked session.
        if let Some(connection) = self.0.as_mut() {
            connection.close_on_drop();
        }
    }
}

impl RuntimeLoopLeader {
    pub async fn try_acquire(
        pool: &PgPool,
        kind: RuntimeLoop,
    ) -> Result<Option<Self>, sqlx::Error> {
        let mut acquisition = AcquiringLoopLock(Some(pool.acquire().await?));
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(kind.lock_id())
            .fetch_one(&mut **acquisition.0.as_mut().expect("acquiring connection"))
            .await?;
        // A known loser can return its unlocked connection to the pool.
        let mut connection = acquisition.0.take().expect("acquiring connection");
        if !acquired {
            return Ok(None);
        }
        connection.close_on_drop();
        Ok(Some(Self { connection }))
    }

    pub async fn check(&mut self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1")
            .execute(&mut *self.connection)
            .await?;
        Ok(())
    }
}

fn lease_millis(lease: Duration) -> Result<i64, PgStoreError> {
    i64::try_from(lease.as_millis())
        .ok()
        .filter(|millis| *millis > 0)
        .ok_or_else(|| PgStoreError::Store {
            message: "runtime instance lease must be a positive number of milliseconds fitting i64"
                .to_owned(),
        })
}

fn instance_from_row(row: &PgRow) -> Result<RuntimeInstance, sqlx::Error> {
    Ok(RuntimeInstance {
        instance_id: row.try_get("instance_id")?,
        roles: row.try_get("roles")?,
        peer_url: row.try_get("peer_url")?,
        started_at_ms: row.try_get("started_at_ms")?,
        lease_expires_at_ms: row.try_get("lease_expires_at_ms")?,
    })
}

fn owner_from_row(row: &PgRow) -> Result<RegisteredConnectionOwner, sqlx::Error> {
    Ok(RegisteredConnectionOwner {
        owner_instance_id: row.try_get("owner_instance_id")?,
        control_connection_id: row.try_get("control_connection_id")?,
        peer_url: row.try_get("peer_url")?,
    })
}
