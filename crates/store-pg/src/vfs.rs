use async_trait::async_trait;
use engine::{BlobRef, SessionId};
use sqlx::Row;

use crate::{PgStore, shared::sha256_hex};

use ::vfs::{
    CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountAccess,
    VfsMountRecord, VfsMountSource, VfsMountStore, VfsPath, VfsSnapshotRecord, VfsSnapshotSource,
    VfsSnapshotStore, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore,
};

#[async_trait]
impl VfsSnapshotStore for PgStore {
    async fn record_snapshot(&self, record: VfsSnapshotRecord) -> Result<(), VfsCatalogError> {
        self.ensure_universe()
            .await
            .map_err(|error| catalog_store_error("ensure universe", error))?;
        let source_json =
            serde_json::to_value(&record.source).map_err(|error| VfsCatalogError::Store {
                message: format!("serialize vfs snapshot source: {error}"),
            })?;
        sqlx::query(
            r#"
            INSERT INTO vfs_snapshots (
                universe_id,
                digest,
                source_json,
                display_name,
                created_at_ms
            )
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (universe_id, digest) DO UPDATE
            SET
                source_json = EXCLUDED.source_json,
                display_name = EXCLUDED.display_name,
                created_at_ms = EXCLUDED.created_at_ms,
                modified_at = now()
            "#,
        )
        .bind(self.config.universe_id)
        .bind(catalog_digest(&record.snapshot_ref)?)
        .bind(source_json)
        .bind(record.display_name.as_deref())
        .bind(nonnegative_i64(record.created_at_ms, "created_at_ms")?)
        .execute(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("record vfs snapshot", error))?;
        Ok(())
    }

    async fn read_snapshot(
        &self,
        snapshot_ref: &BlobRef,
    ) -> Result<VfsSnapshotRecord, VfsCatalogError> {
        let row = sqlx::query(
            r#"
            SELECT digest, source_json, display_name, created_at_ms
            FROM vfs_snapshots
            WHERE universe_id = $1 AND digest = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(catalog_digest(snapshot_ref)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("read vfs snapshot", error))?;

        let Some(row) = row else {
            return Err(VfsCatalogError::NotFound {
                kind: "snapshot",
                id: snapshot_ref.to_string(),
            });
        };
        snapshot_record_from_row(&row)
    }
}

#[async_trait]
impl VfsWorkspaceStore for PgStore {
    async fn create_workspace(
        &self,
        record: CreateVfsWorkspaceRecord,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        self.ensure_universe()
            .await
            .map_err(|error| catalog_store_error("ensure universe", error))?;
        let base_digest = record
            .base_snapshot_ref
            .as_ref()
            .map(catalog_digest)
            .transpose()?;
        let row = sqlx::query(
            r#"
            INSERT INTO vfs_workspaces (
                universe_id,
                workspace_id,
                base_snapshot_digest,
                head_snapshot_digest,
                revision,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, 0, $5, $5)
            ON CONFLICT (universe_id, workspace_id) DO NOTHING
            RETURNING
                workspace_id,
                base_snapshot_digest,
                head_snapshot_digest,
                revision,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.workspace_id.as_str())
        .bind(base_digest)
        .bind(catalog_digest(&record.head_snapshot_ref)?)
        .bind(nonnegative_i64(record.created_at_ms, "created_at_ms")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("create vfs workspace", error))?;

        let Some(row) = row else {
            return Err(VfsCatalogError::AlreadyExists {
                kind: "workspace",
                id: record.workspace_id.to_string(),
            });
        };
        workspace_record_from_row(&row)
    }

    async fn read_workspace(
        &self,
        workspace_id: &VfsWorkspaceId,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let row = sqlx::query(
            r#"
            SELECT
                workspace_id,
                base_snapshot_digest,
                head_snapshot_digest,
                revision,
                created_at_ms,
                updated_at_ms
            FROM vfs_workspaces
            WHERE universe_id = $1 AND workspace_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(workspace_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("read vfs workspace", error))?;

        let Some(row) = row else {
            return Err(VfsCatalogError::NotFound {
                kind: "workspace",
                id: workspace_id.to_string(),
            });
        };
        workspace_record_from_row(&row)
    }

    async fn compare_and_set_head(
        &self,
        request: CompareAndSetVfsWorkspaceHead,
    ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| catalog_sql_error("begin vfs workspace transaction", error))?;
        let row = sqlx::query(
            r#"
            SELECT
                workspace_id,
                base_snapshot_digest,
                head_snapshot_digest,
                revision,
                created_at_ms,
                updated_at_ms
            FROM vfs_workspaces
            WHERE universe_id = $1 AND workspace_id = $2
            FOR UPDATE
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.workspace_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| catalog_sql_error("read vfs workspace for update", error))?;

        let Some(row) = row else {
            return Err(VfsCatalogError::NotFound {
                kind: "workspace",
                id: request.workspace_id.to_string(),
            });
        };
        let current = workspace_record_from_row(&row)?;
        if current.revision != request.expected_revision {
            return Err(VfsCatalogError::RevisionConflict {
                workspace_id: request.workspace_id,
                expected_revision: request.expected_revision,
                actual_revision: current.revision,
            });
        }
        let next_revision =
            current
                .revision
                .checked_add(1)
                .ok_or_else(|| VfsCatalogError::Store {
                    message: format!(
                        "vfs workspace revision overflow for {}",
                        current.workspace_id
                    ),
                })?;
        let row = sqlx::query(
            r#"
            UPDATE vfs_workspaces
            SET
                head_snapshot_digest = $3,
                revision = $4,
                updated_at_ms = $5,
                modified_at = now()
            WHERE universe_id = $1 AND workspace_id = $2
            RETURNING
                workspace_id,
                base_snapshot_digest,
                head_snapshot_digest,
                revision,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(current.workspace_id.as_str())
        .bind(catalog_digest(&request.new_head_snapshot_ref)?)
        .bind(u64_to_i64(next_revision, "revision")?)
        .bind(nonnegative_i64(request.updated_at_ms, "updated_at_ms")?)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| catalog_sql_error("advance vfs workspace head", error))?;
        tx.commit()
            .await
            .map_err(|error| catalog_sql_error("commit vfs workspace transaction", error))?;
        workspace_record_from_row(&row)
    }
}

#[async_trait]
impl VfsMountStore for PgStore {
    async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
        self.ensure_universe()
            .await
            .map_err(|error| catalog_store_error("ensure universe", error))?;
        let (source_kind, snapshot_digest, workspace_id) = mount_source_columns(&record.source)?;
        sqlx::query(
            r#"
            INSERT INTO vfs_mounts (
                universe_id,
                session_id,
                mount_path,
                source_kind,
                snapshot_digest,
                workspace_id,
                access
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (universe_id, session_id, mount_path) DO UPDATE
            SET
                source_kind = EXCLUDED.source_kind,
                snapshot_digest = EXCLUDED.snapshot_digest,
                workspace_id = EXCLUDED.workspace_id,
                access = EXCLUDED.access,
                modified_at = now()
            "#,
        )
        .bind(self.config.universe_id)
        .bind(record.session_id.as_str())
        .bind(record.mount_path.as_str())
        .bind(source_kind)
        .bind(snapshot_digest)
        .bind(workspace_id)
        .bind(mount_access_to_str(record.access))
        .execute(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("put vfs mount", error))?;
        Ok(())
    }

    async fn list_mounts(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
        let rows = sqlx::query(
            r#"
            SELECT
                session_id,
                mount_path,
                source_kind,
                snapshot_digest,
                workspace_id,
                access
            FROM vfs_mounts
            WHERE universe_id = $1 AND session_id = $2
            ORDER BY mount_path
            "#,
        )
        .bind(self.config.universe_id)
        .bind(session_id.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("list vfs mounts", error))?;

        rows.iter().map(mount_record_from_row).collect()
    }

    async fn remove_mount(
        &self,
        session_id: &SessionId,
        mount_path: &VfsPath,
    ) -> Result<(), VfsCatalogError> {
        let result = sqlx::query(
            r#"
            DELETE FROM vfs_mounts
            WHERE universe_id = $1 AND session_id = $2 AND mount_path = $3
            "#,
        )
        .bind(self.config.universe_id)
        .bind(session_id.as_str())
        .bind(mount_path.as_str())
        .execute(&self.pool)
        .await
        .map_err(|error| catalog_sql_error("remove vfs mount", error))?;
        if result.rows_affected() == 0 {
            return Err(VfsCatalogError::NotFound {
                kind: "mount",
                id: format!("{session_id}:{mount_path}"),
            });
        }
        Ok(())
    }
}

fn snapshot_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<VfsSnapshotRecord, VfsCatalogError> {
    let digest: String = row
        .try_get("digest")
        .map_err(|error| catalog_sql_error("decode vfs snapshot digest", error))?;
    let source_json: serde_json::Value = row
        .try_get("source_json")
        .map_err(|error| catalog_sql_error("decode vfs snapshot source", error))?;
    let source: VfsSnapshotSource =
        serde_json::from_value(source_json).map_err(|error| VfsCatalogError::Store {
            message: format!("decode vfs snapshot source: {error}"),
        })?;
    let display_name = row
        .try_get("display_name")
        .map_err(|error| catalog_sql_error("decode vfs snapshot display name", error))?;
    let created_at_ms = row
        .try_get("created_at_ms")
        .map_err(|error| catalog_sql_error("decode vfs snapshot created_at_ms", error))?;
    Ok(VfsSnapshotRecord {
        snapshot_ref: blob_ref_from_digest(&digest)?,
        source,
        display_name,
        created_at_ms,
    })
}

fn workspace_record_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
    let workspace_id: String = row
        .try_get("workspace_id")
        .map_err(|error| catalog_sql_error("decode vfs workspace id", error))?;
    let base_snapshot_digest: Option<String> = row
        .try_get("base_snapshot_digest")
        .map_err(|error| catalog_sql_error("decode vfs workspace base digest", error))?;
    let head_snapshot_digest: String = row
        .try_get("head_snapshot_digest")
        .map_err(|error| catalog_sql_error("decode vfs workspace head digest", error))?;
    let revision: i64 = row
        .try_get("revision")
        .map_err(|error| catalog_sql_error("decode vfs workspace revision", error))?;
    let created_at_ms = row
        .try_get("created_at_ms")
        .map_err(|error| catalog_sql_error("decode vfs workspace created_at_ms", error))?;
    let updated_at_ms = row
        .try_get("updated_at_ms")
        .map_err(|error| catalog_sql_error("decode vfs workspace updated_at_ms", error))?;
    Ok(VfsWorkspaceRecord {
        workspace_id: VfsWorkspaceId::try_new(workspace_id).map_err(|error| {
            VfsCatalogError::Store {
                message: format!("decode vfs workspace id: {error}"),
            }
        })?,
        base_snapshot_ref: base_snapshot_digest
            .as_deref()
            .map(blob_ref_from_digest)
            .transpose()?,
        head_snapshot_ref: blob_ref_from_digest(&head_snapshot_digest)?,
        revision: i64_to_u64(revision, "revision")?,
        created_at_ms,
        updated_at_ms,
    })
}

fn mount_record_from_row(row: &sqlx::postgres::PgRow) -> Result<VfsMountRecord, VfsCatalogError> {
    let session_id: String = row
        .try_get("session_id")
        .map_err(|error| catalog_sql_error("decode vfs mount session id", error))?;
    let mount_path: String = row
        .try_get("mount_path")
        .map_err(|error| catalog_sql_error("decode vfs mount path", error))?;
    let source_kind: String = row
        .try_get("source_kind")
        .map_err(|error| catalog_sql_error("decode vfs mount source kind", error))?;
    let snapshot_digest: Option<String> = row
        .try_get("snapshot_digest")
        .map_err(|error| catalog_sql_error("decode vfs mount snapshot digest", error))?;
    let workspace_id: Option<String> = row
        .try_get("workspace_id")
        .map_err(|error| catalog_sql_error("decode vfs mount workspace id", error))?;
    let access: String = row
        .try_get("access")
        .map_err(|error| catalog_sql_error("decode vfs mount access", error))?;

    Ok(VfsMountRecord {
        session_id: SessionId::try_new(session_id).map_err(|error| VfsCatalogError::Store {
            message: format!("decode vfs mount session id: {error}"),
        })?,
        mount_path: VfsPath::parse(mount_path).map_err(|error| VfsCatalogError::Store {
            message: format!("decode vfs mount path: {error}"),
        })?,
        source: match source_kind.as_str() {
            "snapshot" => VfsMountSource::Snapshot {
                snapshot_ref: blob_ref_from_digest(snapshot_digest.as_deref().ok_or_else(
                    || VfsCatalogError::Store {
                        message: "vfs snapshot mount row has no snapshot digest".to_string(),
                    },
                )?)?,
            },
            "workspace" => VfsMountSource::Workspace {
                workspace_id: VfsWorkspaceId::try_new(workspace_id.ok_or_else(|| {
                    VfsCatalogError::Store {
                        message: "vfs workspace mount row has no workspace id".to_string(),
                    }
                })?)
                .map_err(|error| VfsCatalogError::Store {
                    message: format!("decode vfs mount workspace id: {error}"),
                })?,
            },
            other => {
                return Err(VfsCatalogError::Store {
                    message: format!("unsupported vfs mount source kind '{other}'"),
                });
            }
        },
        access: mount_access_from_str(&access)?,
    })
}

fn mount_source_columns(
    source: &VfsMountSource,
) -> Result<(&'static str, Option<&str>, Option<&str>), VfsCatalogError> {
    match source {
        VfsMountSource::Snapshot { snapshot_ref } => {
            Ok(("snapshot", Some(catalog_digest(snapshot_ref)?), None))
        }
        VfsMountSource::Workspace { workspace_id } => {
            Ok(("workspace", None, Some(workspace_id.as_str())))
        }
    }
}

fn mount_access_to_str(access: VfsMountAccess) -> &'static str {
    match access {
        VfsMountAccess::ReadOnly => "read_only",
        VfsMountAccess::ReadWrite => "read_write",
    }
}

fn mount_access_from_str(value: &str) -> Result<VfsMountAccess, VfsCatalogError> {
    match value {
        "read_only" => Ok(VfsMountAccess::ReadOnly),
        "read_write" => Ok(VfsMountAccess::ReadWrite),
        other => Err(VfsCatalogError::Store {
            message: format!("unsupported vfs mount access '{other}'"),
        }),
    }
}

fn catalog_digest(blob_ref: &BlobRef) -> Result<&str, VfsCatalogError> {
    sha256_hex(blob_ref).map_err(|error| VfsCatalogError::Store {
        message: error.to_string(),
    })
}

fn blob_ref_from_digest(digest: &str) -> Result<BlobRef, VfsCatalogError> {
    BlobRef::parse(format!("sha256:{digest}")).map_err(|error| VfsCatalogError::Store {
        message: format!("decode blob ref digest '{digest}': {error}"),
    })
}

fn u64_to_i64(value: u64, name: &str) -> Result<i64, VfsCatalogError> {
    i64::try_from(value).map_err(|_| VfsCatalogError::Store {
        message: format!("{name} is too large for Postgres bigint: {value}"),
    })
}

fn i64_to_u64(value: i64, name: &str) -> Result<u64, VfsCatalogError> {
    u64::try_from(value).map_err(|_| VfsCatalogError::Store {
        message: format!("{name} is negative in Postgres: {value}"),
    })
}

fn nonnegative_i64(value: i64, name: &str) -> Result<i64, VfsCatalogError> {
    if value < 0 {
        return Err(VfsCatalogError::InvalidInput {
            message: format!("{name} must be nonnegative: {value}"),
        });
    }
    Ok(value)
}

fn catalog_store_error(action: &str, error: crate::PgStoreError) -> VfsCatalogError {
    VfsCatalogError::Store {
        message: format!("{action}: {error}"),
    }
}

fn catalog_sql_error(action: &str, error: sqlx::Error) -> VfsCatalogError {
    VfsCatalogError::Store {
        message: format!("{action}: {error}"),
    }
}
